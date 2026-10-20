//! The sender session: one receiver, one virtual display, capture -> encode ->
//! wire, driven by `SenderSession` (§1 sender dials, §5, §6, §8). Talks to
//! the compositor only through [`Backend`] / [`VirtualDisplay`] /
//! [`InputSink`].

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use opendisplay_proto::control::{CursorImg, Hello, StreamConfig};
use opendisplay_session::Now;
use opendisplay_session::sender::{SenderAction, SenderConfig, SenderEvent, SenderSession};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::backend::lock::{self, LockHeld};
use crate::backend::{
    Backend, CaptureEvent, CaptureOptions, CursorEvent, DisplayInfo, DisplaySpec, Frame, FrameSink,
    InputSink, Position, VirtualDisplay,
};
use crate::config::{self, Config, Layer};
use crate::desktop::{self, Debounce, DesktopOptions, DesktopPlan, PlanError, StreamPlan};
use crate::encoder::{Encoded, Encoder, EncoderSettings};
use crate::latest;
use crate::wayland::cursor::base64;

/// How the pointer cursor reaches the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum CursorMode {
    /// On the cursor side channel (`cursor`/`cursorImg`, PROTOCOL §6.3) when
    /// the receiver offers a `cursorPort` and the compositor can deliver the
    /// cursor separately; painted into the video otherwise.
    #[default]
    Auto,
    /// Always painted into the video.
    Video,
    /// Not shown.
    Off,
}

/// Whether to ask the display for the cursor on its own: `Auto`, a backend
/// that can, and a receiver that renders a cursor (it offers `cursorPort`).
fn cursor_separately(mode: CursorMode, backend_can: bool, receiver_offers: bool) -> bool {
    mode == CursorMode::Auto && backend_can && receiver_offers
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub bitrate_mbps: f32,
    /// Output scale policy and frame-rate cap for [`desktop::plan`].
    pub desktop: DesktopOptions,
    pub threads: u16,
    pub encoder: crate::encoder::Backend,
    /// How the cursor reaches the receiver.
    pub cursor: CursorMode,
    /// Where the virtual output goes.
    pub position: Position,
    /// Leave the virtual output in place when the session ends.
    pub keep_output: bool,
    pub reconnect: bool,
    pub duration: Option<Duration>,
    /// The receiver's id when discovery told us, so a lock conflict is found
    /// before dialing (the receiver hands itself to whoever connects last).
    pub receiver_id: Option<String>,
    /// How the receiver is named in messages before its `hello` arrives.
    pub receiver_name: String,
    /// What the command line set explicitly; wins over the config file.
    pub cli: Layer,
    /// The config file, for `[receiver."…"]` overrides.
    pub config: Config,
}

impl RunOptions {
    /// These options with the config file's table for this receiver applied
    /// under the command line. `keys`: id, Bonjour name, device name.
    pub fn for_receiver(&self, keys: &[Option<&str>]) -> RunOptions {
        let Some((key, o)) = self.config.receiver(keys) else {
            return self.clone();
        };
        debug!("config: [receiver.\"{key}\"] applies");
        let eff = self
            .cli
            .over(o)
            .over(&self.config.defaults)
            .over(&config::builtin());
        RunOptions {
            position: eff.position.unwrap_or(self.position),
            bitrate_mbps: eff.bitrate_mbps.unwrap_or(self.bitrate_mbps),
            desktop: DesktopOptions {
                scale: eff.scale.unwrap_or(self.desktop.scale),
                ..self.desktop
            },
            ..self.clone()
        }
    }
}

fn now(start: Instant) -> Now {
    let wall_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    Now::new(start.elapsed(), wall_ms)
}

/// The output for a receiver with this `hello.id` (equal to its Bonjour TXT
/// `id`, PROTOCOL §2.1): `od-<first 6 chars>`; also the lock key.
pub fn output_name(id: Option<&str>) -> String {
    let short = id
        .map(|s| s.chars().take(6).collect::<String>().to_lowercase())
        .unwrap_or_else(|| "anon".into());
    format!("od-{short}")
}

/// What a `hello` asks the backend for: the desktop plan plus where and
/// under which name the output goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub spec: DisplaySpec,
    pub plan: DesktopPlan,
}

pub fn target(hello: &Hello, opts: &RunOptions) -> Result<Target, PlanError> {
    let plan = desktop::plan(hello, &opts.desktop)?;
    Ok(Target {
        spec: DisplaySpec {
            name: output_name(hello.id.as_deref()),
            width: plan.output.width,
            height: plan.output.height,
            hz: plan.output.hz,
            scale: plan.output.scale,
            position: opts.position,
        },
        plan,
    })
}

/// The stream to announce for the display as built. A backend that cannot
/// size its output (adopt) streams the output as it is.
fn stream_for(info: &DisplayInfo, target: &Target) -> StreamPlan {
    let s = target.plan.stream;
    if (info.width, info.height) == (target.spec.width, target.spec.height) {
        s
    } else {
        StreamPlan {
            width: info.width & !1,
            height: info.height & !1,
            ..s
        }
    }
}

/// How often a display that vanished under the session (output removed,
/// capture stopped by the compositor) is recreated before giving up.
pub struct LossBudget {
    window: Duration,
    max: usize,
    losses: VecDeque<Instant>,
}

impl Default for LossBudget {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            max: 3,
            losses: VecDeque::new(),
        }
    }
}

impl LossBudget {
    /// Record a loss at `at`; `false` once more than `max` fall in `window`.
    pub fn allow(&mut self, at: Instant) -> bool {
        while self
            .losses
            .front()
            .is_some_and(|&t| at.duration_since(t) > self.window)
        {
            self.losses.pop_front();
        }
        self.losses.push_back(at);
        self.losses.len() <= self.max
    }
}

/// Spawn a pipeline thread whose panic is reported on `lost` like a lost
/// display, so the session rebuilds (or gives up) instead of waiting on a
/// channel nobody feeds.
fn spawn_reporting_panics(
    name: &str,
    lost: mpsc::UnboundedSender<String>,
    f: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    struct Report(String, mpsc::UnboundedSender<String>);
    impl Drop for Report {
        fn drop(&mut self) {
            if std::thread::panicking() {
                let _ = self.1.send(format!("{} thread panicked", self.0));
            }
        }
    }
    let report = Report(name.to_string(), lost);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _report = report;
            f();
        })
}

/// Encoder thread fed by a capture [`FrameSink`]: latest frame wins, rate
/// capped, last frame replayed as an IDR on a static screen (§5.3).
struct EncodePipeline {
    stop: Arc<AtomicBool>,
    force_idr: Arc<AtomicBool>,
    raw_tx: Option<latest::Sender<Frame>>,
    encoded_rx: mpsc::Receiver<Encoded>,
    lost_tx: mpsc::UnboundedSender<String>,
    lost_rx: mpsc::UnboundedReceiver<String>,
}

impl EncodePipeline {
    fn start(mut settings: EncoderSettings) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let force_idr = Arc::new(AtomicBool::new(true));
        let (raw_tx, raw_rx) = latest::channel::<Frame>();
        let (encoded_tx, encoded_rx) = mpsc::channel::<Encoded>(4);
        let (lost_tx, lost_rx) = mpsc::unbounded_channel();
        let enc_stop = stop.clone();
        let enc_force = force_idr.clone();
        spawn_reporting_panics("encode", lost_tx.clone(), move || {
            let mut encoder: Option<Encoder> = None;
            let mut last: Option<Frame> = None;
            // Newest captured frame not yet encoded. A frame that arrives
            // inside the rate cap waits here for its slot rather than
            // being dropped: on a screen that then goes static it is the
            // last picture, and no further damage would ever replace it.
            let mut pending: Option<Frame> = None;
            let min_interval = Duration::from_secs_f32(1.0 / settings.max_fps.max(1.0));
            // The cap is measured between encode starts, so encode time
            // counts towards the interval instead of adding to it.
            let mut last_start = Instant::now() - min_interval;
            while !enc_stop.load(Ordering::Relaxed) {
                let timeout = if pending.is_some() {
                    min_interval.saturating_sub(last_start.elapsed())
                } else {
                    Duration::from_millis(100)
                };
                match raw_rx.recv_timeout(timeout) {
                    Ok(f) => pending = Some(f),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
                let frame = if pending.is_some() {
                    if last_start.elapsed() < min_interval {
                        continue;
                    }
                    pending.take().unwrap()
                } else if enc_force.load(Ordering::Relaxed) && last.is_some() {
                    // Static screen and an IDR was asked for: replay (§5.3).
                    last.take().unwrap()
                } else {
                    continue;
                };
                last_start = Instant::now();
                let (w, h) = frame.dimensions();
                if encoder
                    .as_ref()
                    .is_none_or(|e| e.dimensions() != (w & !1, h & !1))
                {
                    match Encoder::new(w, h, settings) {
                        Ok(e) => {
                            info!(
                                "encoder: {} {}x{} {:.1} Mbit/s, {} threads",
                                e.label(),
                                w & !1,
                                h & !1,
                                settings.bitrate_bps as f64 / 1e6,
                                settings.threads
                            );
                            // A resize reopens the same backend without probing.
                            settings.backend = e.backend();
                            encoder = Some(e);
                            enc_force.store(true, Ordering::Relaxed);
                        }
                        Err(e) => {
                            warn!("encoder init failed: {e:#}");
                            continue;
                        }
                    }
                }
                let force = enc_force.swap(false, Ordering::Relaxed);
                match encoder.as_mut().unwrap().encode(&frame, force) {
                    Ok(units) => {
                        if force && !units.iter().any(|u| u.unit.is_idr) {
                            warn!("encoder ignored the IDR request");
                            enc_force.store(true, Ordering::Relaxed);
                        }
                        if units
                            .into_iter()
                            .any(|enc| encoded_tx.blocking_send(enc).is_err())
                        {
                            break;
                        }
                    }
                    Err(e) => warn!("encode failed: {e:#}"),
                }
                last = Some(frame);
            }
            debug!("encoder thread done");
        })?;
        Ok(Self {
            stop,
            force_idr,
            raw_tx: Some(raw_tx),
            encoded_rx,
            lost_tx,
            lost_rx,
        })
    }

    /// The capture side: a frame replaces one the busy encoder has not
    /// taken yet, so encoding starts on the newest picture; reports the end
    /// of capture to the session.
    fn sink(&mut self) -> FrameSink {
        let raw_tx = self.raw_tx.take().expect("one sink per pipeline");
        let lost_tx = self.lost_tx.clone();
        Box::new(move |ev| match ev {
            CaptureEvent::Frame(f) => {
                raw_tx.send(f);
            }
            CaptureEvent::Ended(why) => {
                let _ = lost_tx.send(why);
            }
        })
    }
}

impl Drop for EncodePipeline {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The live display with its encoder and input.
struct Active {
    input: Option<Box<dyn InputSink>>,
    display: Box<dyn VirtualDisplay>,
    pipe: EncodePipeline,
    target: Target,
}

impl Active {
    /// Input first, then the display (capture stops before the output goes),
    /// then the encoder.
    fn close(self, keep_output: bool) {
        let Active {
            input,
            display,
            pipe,
            ..
        } = self;
        drop(input);
        let name = display.info().name.clone();
        if let Err(e) = display.close(keep_output) {
            warn!("closing {name}: {e:#}");
        }
        drop(pipe);
    }
}

/// Open (or resize in place) the display for `plan`, then start capture,
/// the encoder and input. On error nothing is active; a display dropped on
/// the way stops its capture and removes its output.
fn build(
    backend: &mut dyn Backend,
    active: &mut Option<Active>,
    target: Target,
    opts: &RunOptions,
    cursor: Option<&mpsc::UnboundedSender<CursorEvent>>,
) -> Result<StreamPlan> {
    let mut display = match active.take() {
        Some(Active {
            input,
            mut display,
            pipe,
            target: old,
        }) => {
            drop(input);
            display.stop_capture();
            drop(pipe);
            if old.spec.name == target.spec.name {
                display.reconfigure(&target.spec)?;
                display
            } else {
                if let Err(e) = display.close(opts.keep_output) {
                    warn!("closing {}: {e:#}", old.spec.name);
                }
                backend.open(&target.spec)?
            }
        }
        None => backend.open(&target.spec)?,
    };
    let mut pipe = EncodePipeline::start(EncoderSettings {
        backend: opts.encoder,
        bitrate_bps: (opts.bitrate_mbps * 1e6) as u32,
        max_fps: target.plan.stream.fps as f32,
        threads: opts.threads,
    })?;
    let mut painted = opts.cursor != CursorMode::Off;
    if let Some(tx) = cursor {
        let tx = tx.clone();
        match display.start_cursor(Box::new(move |ev| {
            let _ = tx.send(ev);
        })) {
            Ok(()) => painted = false,
            Err(e) => {
                warn!("cursor side channel unavailable ({e:#}); painting the cursor into the video")
            }
        }
    }
    display.start_capture(CaptureOptions { cursor: painted }, pipe.sink())?;
    let input = match display.input() {
        Ok(i) => Some(i),
        Err(e) => {
            warn!("input injection unavailable: {e:#}");
            None
        }
    };
    let stream = stream_for(display.info(), &target);
    *active = Some(Active {
        input,
        display,
        pipe,
        target,
    });
    Ok(stream)
}

/// Resolves to `true` on SIGINT, SIGTERM or SIGHUP. Handled explicitly so the output
/// is torn down in order, and because a sender started in the background by
/// a non-interactive shell inherits SIGINT as ignored.
fn shutdown_signal() -> Result<watch::Receiver<bool>> {
    use tokio::signal::unix::{SignalKind, signal};
    let (tx, rx) = watch::channel(false);
    let mut int = signal(SignalKind::interrupt())?;
    let mut term = signal(SignalKind::terminate())?;
    // The terminal closing (or a systemd reload) must not kill us mid-capture.
    let mut hup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        let name = tokio::select! {
            _ = int.recv() => "SIGINT",
            _ = term.recv() => "SIGTERM",
            _ = hup.recv() => "SIGHUP",
        };
        info!("{name}: shutting down");
        let _ = tx.send(true);
    });
    Ok(rx)
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// Another sender drives this receiver's output: stop, do not redial
    /// (each redial would take the receiver from the other sender).
    LockConflict { held: LockHeld, receiver: String },
    /// Connection dropped or shutdown; redial if allowed.
    Ended,
    /// The receiver said `closing`: do not redial.
    ReceiverClosed,
    /// The display kept vanishing or could not be recreated: stop.
    DisplayLost,
    /// No stream this sender can produce suits the receiver: stop.
    Incompatible,
}

/// Redial delay: doubles from `min` to `max` while sessions keep failing;
/// back to `min` after a session that lasted `healthy`.
pub struct Backoff {
    min: Duration,
    max: Duration,
    healthy: Duration,
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(
            Duration::from_millis(500),
            Duration::from_secs(10),
            Duration::from_secs(30),
        )
    }
}

impl Backoff {
    pub fn new(min: Duration, max: Duration, healthy: Duration) -> Self {
        Self {
            min,
            max,
            healthy,
            next: min,
        }
    }

    /// The delay before the next dial. `lasted` is how long the session
    /// that just ended ran (`None` when the dial itself failed).
    pub fn delay(&mut self, lasted: Option<Duration>) -> Duration {
        if lasted.is_some_and(|l| l >= self.healthy) {
            self.next = self.min;
        }
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        d
    }
}

/// How a receiver is named in messages: device and id from its `hello`.
fn receiver_label(h: &Hello) -> String {
    match (&h.device, &h.id) {
        (Some(d), Some(id)) => format!("{d} ({id})"),
        (Some(d), None) => d.clone(),
        (None, Some(id)) => id.clone(),
        (None, None) => "this receiver".into(),
    }
}

/// `Some` when the lock `spec` needs is held by another process. Our own
/// display holding it (a rebuild for the same receiver) is no conflict.
fn lock_conflict(
    backend: &dyn Backend,
    active: Option<&Active>,
    spec: &DisplaySpec,
) -> Result<Option<LockHeld>> {
    let name = backend.lock_name(spec);
    if active.is_some_and(|a| backend.lock_name(&a.target.spec) == name) {
        return Ok(None);
    }
    Ok(match lock::try_lock(&name)? {
        Some(_probe) => None,
        None => Some(LockHeld {
            pid: lock::holder_pid(&name),
            name,
        }),
    })
}

/// Before dialing: `Some` when the output for receiver `id` is already
/// driven by another sender.
fn pre_dial_conflict(
    backend: &dyn Backend,
    id: &str,
    position: Position,
) -> Result<Option<LockHeld>> {
    let spec = DisplaySpec {
        name: output_name(Some(id)),
        width: 0,
        height: 0,
        hz: 0,
        scale: 1.0,
        position,
    };
    lock_conflict(backend, None, &spec)
}

/// A lock conflict surfacing from `open` (a sender that started between our
/// check and our open) ends the session like the check would have.
fn conflict_or(e: anyhow::Error, receiver: String) -> Result<SessionEnd> {
    match e.downcast_ref::<LockHeld>() {
        Some(held) => Ok(SessionEnd::LockConflict {
            held: held.clone(),
            receiver,
        }),
        None => Err(e),
    }
}

pub async fn run(addr: SocketAddr, backend: &mut dyn Backend, opts: RunOptions) -> Result<()> {
    let start = Instant::now();
    info!(
        "backend {}: {} ({})",
        backend.id(),
        backend.describe(),
        backend.caps()
    );
    let mut shutdown = shutdown_signal()?;
    let mut losses = LossBudget::default();
    let mut backoff = Backoff::default();
    if let Some(id) = &opts.receiver_id {
        if let Some(held) = pre_dial_conflict(backend, id, opts.position)? {
            anyhow::bail!("{held} for {}; stop it first", opts.receiver_name);
        }
    }
    loop {
        let mut lasted = None;
        if *shutdown.borrow() || opts.duration.is_some_and(|d| start.elapsed() >= d) {
            break;
        }
        let dialed = tokio::select! {
            r = TcpStream::connect(addr) => r,
            _ = shutdown.changed() => break,
        };
        match dialed {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                info!("connected to receiver {addr}");
                let began = Instant::now();
                let end = session(
                    stream,
                    backend,
                    &opts,
                    start,
                    shutdown.clone(),
                    &mut losses,
                    Some(addr),
                )
                .await;
                lasted = Some(began.elapsed());
                match end {
                    Ok(SessionEnd::LockConflict { held, receiver }) => {
                        anyhow::bail!("{held} for {receiver}; stop it first");
                    }
                    Ok(SessionEnd::ReceiverClosed) => {
                        info!("receiver closed the session for good");
                        break;
                    }
                    Ok(SessionEnd::Incompatible) => {
                        warn!("giving up: the receiver accepts no stream this sender can produce");
                        break;
                    }
                    Ok(SessionEnd::DisplayLost) => {
                        warn!("giving up: the virtual display could not be kept alive");
                        break;
                    }
                    Ok(SessionEnd::Ended) => info!("connection ended"),
                    Err(e) => warn!("session error: {e:#}"),
                }
            }
            Err(e) => warn!("dial {addr}: {e}"),
        }
        if !opts.reconnect || *shutdown.borrow() {
            break;
        }
        let delay = backoff.delay(lasted);
        debug!("redialing in {delay:?}");
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => break,
        }
    }
    Ok(())
}

enum DisplayEvent {
    Encoded(Encoded),
    Lost(String),
}

async fn next_display_event(active: &mut Option<Active>) -> DisplayEvent {
    let Some(a) = active else {
        return std::future::pending().await;
    };
    tokio::select! {
        Some(e) = a.pipe.encoded_rx.recv() => DisplayEvent::Encoded(e),
        Some(why) = a.pipe.lost_rx.recv() => DisplayEvent::Lost(why),
        // Encoder gone and capture silent: nothing more will come.
        else => std::future::pending().await,
    }
}

async fn send_stream_config<S: AsyncWrite + Unpin>(
    stream: &mut S,
    sess: &SenderSession,
    plan: &StreamPlan,
) -> Result<()> {
    let cfg = sess.stream_config(StreamConfig {
        codec: plan.codec.into(),
        width: plan.width,
        height: plan.height,
        frames_per_second: plan.fps,
    });
    if let SenderAction::Send(b) = cfg {
        stream.write_all(&b).await.context("write")?;
    }
    Ok(())
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// Build the display for `target` and announce its stream.
async fn apply<S: AsyncWrite + Unpin>(
    stream: &mut S,
    sess: &SenderSession,
    backend: &mut dyn Backend,
    active: &mut Option<Active>,
    target: Target,
    opts: &RunOptions,
    cursor: Option<&mpsc::UnboundedSender<CursorEvent>>,
) -> Result<()> {
    info!("plan: {}", target.plan);
    let plan = build(backend, active, target, opts, cursor)?;
    send_stream_config(stream, sess, &plan).await
}

async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    backend: &mut dyn Backend,
    opts: &RunOptions,
    start: Instant,
    mut shutdown: watch::Receiver<bool>,
    losses: &mut LossBudget,
    peer: Option<SocketAddr>,
) -> Result<SessionEnd> {
    let mut sess = SenderSession::new(SenderConfig::default());
    // Cursor events from the display's cursor thread, and the UDP socket for
    // the side channel (bound on the first datagram).
    let (cursor_tx, mut cursor_rx) = mpsc::unbounded_channel::<CursorEvent>();
    let mut udp: Option<tokio::net::UdpSocket> = None;
    let cursor_for = |opts: &RunOptions, backend: &dyn Backend, sess: &SenderSession| {
        let offers = sess.hello().is_some_and(|h| h.cursor_port.is_some());
        cursor_separately(opts.cursor, backend.caps().cursor_metadata, offers)
            .then(|| cursor_tx.clone())
    };
    sess.handle(now(start), SenderEvent::Connected);
    let mut active: Option<Active> = None;
    // `opts` with this receiver's config table applied, once its hello names it.
    let mut ropts = opts.clone();
    let mut debounce: Debounce<Target> = Debounce::new(desktop::HELLO_DEBOUNCE);
    let mut buf = vec![0u8; 64 * 1024];
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut stats_tick = tokio::time::interval(Duration::from_secs(5));
    stats_tick.reset();
    let (mut win_frames, mut win_bytes, mut win_enc_ms, mut win_lat_ms) =
        (0u64, 0u64, Vec::<f64>::new(), Vec::<f64>::new());

    let result: Result<SessionEnd> = async {
        loop {
            let actions: Vec<SenderAction> = tokio::select! {
                _ = shutdown.changed() => return Ok(SessionEnd::Ended),
                r = stream.read(&mut buf) => {
                    let n = r.context("read")?;
                    if n == 0 { return Ok(SessionEnd::Ended); }
                    sess.handle(now(start), SenderEvent::Bytes(&buf[..n]))
                }
                ev = next_display_event(&mut active) => match ev {
                    DisplayEvent::Encoded(enc) => {
                        let n = now(start);
                        let cap_wall = n.wall_ms - enc.captured_at.elapsed().as_secs_f64() * 1000.0;
                        let a = sess.push_encoded_frame(n, &enc.unit.annexb, enc.unit.is_idr, Some(cap_wall));
                        if !a.is_empty() {
                            win_frames += 1;
                            win_bytes += enc.unit.annexb.len() as u64;
                            win_enc_ms.push(enc.encode_ms);
                            win_lat_ms.push(enc.captured_at.elapsed().as_secs_f64() * 1000.0);
                        }
                        a
                    }
                    DisplayEvent::Lost(why) => {
                        let target = active.as_ref().map(|a| a.target.clone()).expect("lost implies active");
                        let name = target.spec.name.clone();
                        warn!("virtual display {name} lost: {why}");
                        if !losses.allow(Instant::now()) {
                            warn!("{name} lost too often; closing the connection");
                            return Ok(SessionEnd::DisplayLost);
                        }
                        let cur = cursor_for(&ropts, backend, &sess);
                        match build(backend, &mut active, target, &ropts, cur.as_ref()) {
                            Ok(plan) => {
                                info!("recreated virtual display {name}");
                                send_stream_config(&mut stream, &sess, &plan).await?;
                            }
                            Err(e) => {
                                warn!("cannot recreate the virtual display: {e:#}; closing the connection");
                                return Ok(SessionEnd::DisplayLost);
                            }
                        }
                        vec![]
                    }
                },
                Some(ev) = cursor_rx.recv() => match ev {
                    CursorEvent::Moved { x, y } => sess.cursor(now(start), x, y, true),
                    CursorEvent::Hidden => sess.cursor(now(start), 0.0, 0.0, false),
                    CursorEvent::Image { png, nw, nh, ax, ay } => {
                        vec![sess.cursor_image(CursorImg { nw, nh, ax, ay, png: base64(&png) })]
                    }
                },
                _ = sleep_until_opt(debounce.deadline()) => {
                    if let Some(t) = debounce.poll(Instant::now()) {
                        let cur = cursor_for(&ropts, backend, &sess);
                        if let Err(e) = apply(&mut stream, &sess, backend, &mut active, t, &ropts, cur.as_ref()).await {
                            return conflict_or(e, sess.hello().map_or_else(|| "this receiver".into(), receiver_label));
                        }
                    }
                    vec![]
                }
                _ = tick.tick() => {
                    if opts.duration.is_some_and(|d| start.elapsed() >= d) { return Ok(SessionEnd::Ended); }
                    sess.handle(now(start), SenderEvent::Tick)
                }
                _ = stats_tick.tick() => {
                    win_enc_ms.sort_by(f64::total_cmp);
                    win_lat_ms.sort_by(f64::total_cmp);
                    let p = |v: &[f64], q: f64| v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
                    info!(
                        "sent {:.1} fps {:.1} Mbit/s | encode p50 {:.1} ms p95 {:.1} ms | capture->send p50 {:.1} ms p95 {:.1} ms | {:?}",
                        win_frames as f64 / 5.0, win_bytes as f64 * 8.0 / 5.0 / 1e6,
                        p(&win_enc_ms, 0.5), p(&win_enc_ms, 0.95), p(&win_lat_ms, 0.5), p(&win_lat_ms, 0.95), sess.counters
                    );
                    win_frames = 0; win_bytes = 0; win_enc_ms.clear(); win_lat_ms.clear();
                    vec![]
                }
            };
            for a in actions {
                match a {
                    SenderAction::Send(bytes) => stream.write_all(&bytes).await.context("write")?,
                    SenderAction::SendCursorDatagram(d) => {
                        if let (Some(peer), Some(port)) = (peer, sess.cursor_udp_port()) {
                            if udp.is_none() {
                                let any: SocketAddr = if peer.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
                                udp = tokio::net::UdpSocket::bind(any).await.map_err(|e| warn!("cursor UDP socket: {e}")).ok();
                            }
                            if let Some(u) = &udp {
                                // Latest wins: a full socket buffer drops this one.
                                let _ = u.try_send_to(&d, SocketAddr::new(peer.ip(), port));
                            }
                        }
                    }
                    SenderAction::Hello(h) => {
                        info!(
                            "hello: {}x{} @{} scale {} device {:?} id {:?} pv {}",
                            h.pixels_wide, h.pixels_high, h.display_max_frame_rate.unwrap_or(60), h.scale, h.device, h.id, sess.receiver_pv()
                        );
                        ropts = opts.for_receiver(&[
                            h.id.as_deref(),
                            Some(opts.receiver_name.as_str()),
                            h.device.as_deref(),
                        ]);
                        let t = match target(&h, &ropts) {
                            Ok(t) => t,
                            Err(e) => {
                                warn!("cannot plan a desktop for this receiver: {e}; closing the connection");
                                return Ok(SessionEnd::Incompatible);
                            }
                        };
                        // Before `welcome` (it follows `Hello` in this batch):
                        // a receiver another sender drives is left alone.
                        if let Some(held) = lock_conflict(backend, active.as_ref(), &t.spec)? {
                            return Ok(SessionEnd::LockConflict { held, receiver: receiver_label(&h) });
                        }
                        // The first plan applies at once; re-sent hellos settle first (§6.1).
                        if let Some(t) = debounce.on_hello(t, Instant::now()) {
                            let cur = cursor_for(&ropts, backend, &sess);
                            if let Err(e) = apply(&mut stream, &sess, backend, &mut active, t, &ropts, cur.as_ref()).await {
                                return conflict_or(e, receiver_label(&h));
                            }
                        }
                    }
                    SenderAction::RequestKeyframe => {
                        if let Some(a) = &active {
                            a.pipe.force_idr.store(true, Ordering::Relaxed);
                        }
                    }
                    SenderAction::Touch(t) => {
                        debug!("touch {:?} {:.3},{:.3}", t.phase, t.x, t.y);
                        if let Some(i) = active.as_mut().and_then(|a| a.input.as_mut()) {
                            i.touch(&t);
                        }
                    }
                    SenderAction::Scroll(s) => {
                        debug!("scroll {} {}", s.dx, s.dy);
                        if let Some(i) = active.as_mut().and_then(|a| a.input.as_mut()) {
                            i.scroll(&s);
                        }
                    }
                    SenderAction::Pencil(p) => {
                        if let Some(i) = active.as_mut().and_then(|a| a.input.as_mut()) {
                            i.pencil(&p);
                        }
                    }
                    SenderAction::Proximity(_) => {}
                    SenderAction::Stats(v) => info!("PHONE-STATS {v}"),
                    SenderAction::ReceiverSleeping => info!("receiver sleeping"),
                    SenderAction::ReceiverClosing => return Ok(SessionEnd::ReceiverClosed),
                    SenderAction::Close(reason) => {
                        warn!("closing: {reason:?}");
                        return Ok(SessionEnd::Ended);
                    }
                    SenderAction::Warn(m) => warn!("{m}"),
                }
            }
            if !sess.is_connected() {
                return Ok(SessionEnd::Ended);
            }
        }
    }
    .await;

    if let Some(a) = active.take() {
        a.close(opts.keep_output);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use opendisplay_session::receiver::{
        ReceiverAction, ReceiverConfig, ReceiverEvent, ReceiverSession,
    };

    use super::*;
    use crate::backend::{BackendId, Caps};

    fn opts() -> RunOptions {
        RunOptions {
            bitrate_mbps: 20.0,
            desktop: DesktopOptions::default(),
            threads: 1,
            encoder: crate::encoder::Backend::Auto,
            cursor: CursorMode::Auto,
            position: Position::AutoRight,
            keep_output: false,
            reconnect: false,
            duration: None,
            receiver_id: None,
            receiver_name: "test".into(),
            cli: Layer::default(),
            config: Config::default(),
        }
    }

    fn hello() -> Hello {
        Hello {
            pixels_wide: 2532,
            pixels_high: 1170,
            scale: 3.0,
            device: Some("iPhone".into()),
            id: Some("3F9A1C77-0000".into()),
            display_max_frame_rate: Some(120),
            ..Default::default()
        }
    }

    #[test]
    fn target_names_the_output_after_the_receiver() {
        let t = target(&hello(), &opts()).unwrap();
        assert_eq!(t.spec.name, "od-3f9a1c");
        assert_eq!(
            (t.spec.width, t.spec.height, t.spec.hz),
            (t.plan.output.width, t.plan.output.height, t.plan.output.hz)
        );
        let anon = target(
            &Hello {
                id: None,
                ..hello()
            },
            &opts(),
        )
        .unwrap();
        assert_eq!(anon.spec.name, "od-anon");
    }

    #[test]
    fn adopted_outputs_stream_at_their_own_size() {
        let t = target(&hello(), &opts()).unwrap();
        let mut info = info_of(&t.spec);
        assert_eq!(stream_for(&info, &t), t.plan.stream);
        (info.width, info.height) = (1921, 1080);
        let s = stream_for(&info, &t);
        assert_eq!((s.width, s.height, s.fps), (1920, 1080, t.plan.stream.fps));
    }

    #[test]
    fn loss_budget_allows_three_per_minute() {
        let mut b = LossBudget::default();
        let t = Instant::now();
        assert!(b.allow(t));
        assert!(b.allow(t + Duration::from_secs(1)));
        assert!(b.allow(t + Duration::from_secs(2)));
        assert!(!b.allow(t + Duration::from_secs(3)));
        // The oldest ones age out.
        assert!(b.allow(t + Duration::from_secs(120)));
    }

    /// What the fake backend records, plus the capture sink so a test can
    /// end capture the way a removed output does.
    #[derive(Clone, Default)]
    struct Shared {
        log: Arc<Mutex<Vec<String>>>,
        sink: Arc<Mutex<Option<FrameSink>>>,
        fail_reconfigure: Arc<AtomicBool>,
        panic_on_input: Arc<AtomicBool>,
        cursor_sink: Arc<Mutex<Option<crate::backend::CursorSink>>>,
        painted: Arc<Mutex<Option<bool>>>,
    }

    impl Shared {
        fn log(&self, s: impl Into<String>) {
            self.log.lock().unwrap().push(s.into());
        }
        fn entries(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    struct FakeBackend(Shared);
    /// The third field: `close` ran.
    struct FakeDisplay(Shared, DisplayInfo, bool);

    impl Drop for FakeDisplay {
        fn drop(&mut self) {
            if !self.2 {
                self.0.log("dropped without close");
            }
        }
    }
    struct FakeInput;

    impl InputSink for FakeInput {
        fn touch(&mut self, _: &opendisplay_proto::control::Touch) {}
        fn scroll(&mut self, _: &opendisplay_proto::control::Scroll) {}
    }

    fn info_of(spec: &DisplaySpec) -> DisplayInfo {
        DisplayInfo {
            name: spec.name.clone(),
            width: spec.width,
            height: spec.height,
            refresh_hz: spec.hz as f64,
            scale: spec.scale,
            x: 0,
            y: 0,
        }
    }

    impl Backend for FakeBackend {
        fn id(&self) -> BackendId {
            BackendId::Adopt
        }
        fn describe(&self) -> String {
            "fake".into()
        }
        fn caps(&self) -> Caps {
            Caps {
                cursor_metadata: true,
                dmabuf: false,
                resize_in_place: true,
                creates_outputs: true,
            }
        }
        fn open(&mut self, spec: &DisplaySpec) -> Result<Box<dyn VirtualDisplay>> {
            self.0.log(format!("open {}", spec.name));
            Ok(Box::new(FakeDisplay(self.0.clone(), info_of(spec), false)))
        }
        fn clean(&mut self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    impl VirtualDisplay for FakeDisplay {
        fn info(&self) -> &DisplayInfo {
            &self.1
        }
        fn start_cursor(&mut self, sink: crate::backend::CursorSink) -> Result<()> {
            self.0.log("start_cursor");
            *self.0.cursor_sink.lock().unwrap() = Some(sink);
            Ok(())
        }
        fn start_capture(&mut self, o: CaptureOptions, sink: FrameSink) -> Result<()> {
            *self.0.painted.lock().unwrap() = Some(o.cursor);
            self.0.log("start_capture");
            *self.0.sink.lock().unwrap() = Some(sink);
            Ok(())
        }
        fn stop_capture(&mut self) {
            if self.0.sink.lock().unwrap().take().is_some() {
                self.0.log("stop_capture");
            }
        }
        fn reconfigure(&mut self, spec: &DisplaySpec) -> Result<()> {
            self.stop_capture();
            self.0.log("reconfigure");
            if self.0.fail_reconfigure.load(Ordering::Relaxed) {
                anyhow::bail!("output is gone for good");
            }
            self.1 = info_of(spec);
            Ok(())
        }
        fn input(&mut self) -> Result<Box<dyn InputSink>> {
            if self.0.panic_on_input.load(Ordering::Relaxed) {
                panic!("injected panic in the session");
            }
            Ok(Box::new(FakeInput))
        }
        fn close(mut self: Box<Self>, keep_output: bool) -> Result<()> {
            self.2 = true;
            self.stop_capture();
            self.0.log(format!("close keep={keep_output}"));
            Ok(())
        }
    }

    /// A receiver `hello` whose output name is unique to `tag` and this
    /// process, because sessions probe the real lock directory and tests run
    /// in parallel. Returns the hello and the output name.
    fn test_hello(tag: &str) -> (Hello, String) {
        let id = format!("{tag}{:06x}-test", std::process::id());
        let name = format!("od-{}", &id[..6]);
        (
            Hello {
                id: Some(id),
                ..hello()
            },
            name,
        )
    }

    fn forget_lock(name: &str) {
        let _ = std::fs::remove_file(lock::dir().join(format!("{name}.lock")));
    }

    /// Drive one session against a `ReceiverSession` over an in-memory pipe.
    /// After each `streamConfig`, capture ends like an output removed under
    /// it, `losses` times; then the receiver hangs up. Returns the backend
    /// log, the receiver's actions and how the session ended.
    async fn drive(
        hello: Hello,
        losses: usize,
        fail_reconfigure: bool,
    ) -> (Vec<String>, Vec<ReceiverAction>, SessionEnd) {
        let shared = Shared::default();
        shared
            .fail_reconfigure
            .store(fail_reconfigure, Ordering::Relaxed);
        drive_with(shared, hello, losses).await
    }

    async fn drive_with(
        shared: Shared,
        hello: Hello,
        losses: usize,
    ) -> (Vec<String>, Vec<ReceiverAction>, SessionEnd) {
        let (sender_io, mut receiver_io) = tokio::io::duplex(1 << 16);
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut backend = FakeBackend(shared.clone());
        let session_task = tokio::spawn(async move {
            let mut budget = LossBudget::default();
            session(
                sender_io,
                &mut backend,
                &opts(),
                Instant::now(),
                shutdown_rx,
                &mut budget,
                None,
            )
            .await
        });

        let mut rx = ReceiverSession::new(ReceiverConfig::new(hello));
        let rx_now = || Now::new(Duration::from_secs(1), 1e6);
        for a in rx.handle(rx_now(), ReceiverEvent::Connected) {
            if let ReceiverAction::Send(b) = a {
                receiver_io.write_all(&b).await.unwrap();
            }
        }
        let mut seen = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut lost = 0;
        loop {
            let n = tokio::time::timeout(Duration::from_secs(5), receiver_io.read(&mut buf))
                .await
                .expect("sender went quiet")
                .unwrap();
            if n == 0 {
                break;
            }
            for a in rx.handle(rx_now(), ReceiverEvent::Bytes(&buf[..n])) {
                let config = matches!(a, ReceiverAction::StreamConfig(_));
                seen.push(a);
                if !config {
                    continue;
                }
                if lost == losses {
                    drop(receiver_io);
                    let end = session_task.await.unwrap().unwrap();
                    return (shared.entries(), seen, end);
                }
                lost += 1;
                let mut sink = shared.sink.lock().unwrap().take().expect("capturing");
                sink(CaptureEvent::Ended("output removed".into()));
            }
        }
        let end = session_task.await.unwrap().unwrap();
        (shared.entries(), seen, end)
    }

    async fn lose_display(
        tag: &str,
        losses: usize,
        fail_reconfigure: bool,
    ) -> (Vec<String>, SessionEnd, String) {
        let (hello, name) = test_hello(tag);
        let (log, _, end) = drive(hello, losses, fail_reconfigure).await;
        forget_lock(&name);
        (log, end, name)
    }

    #[tokio::test]
    async fn a_lost_display_is_recreated_from_the_last_plan() {
        let (log, end, name) = lose_display("la", 1, false).await;
        assert_eq!(end, SessionEnd::Ended);
        assert_eq!(
            log,
            [
                format!("open {name}").as_str(),
                "start_capture",
                "reconfigure",
                "start_capture",
                "stop_capture",
                "close keep=false",
            ]
        );
    }

    #[tokio::test]
    async fn a_display_lost_too_often_ends_the_run() {
        let (log, end, _) = lose_display("lb", 4, false).await;
        assert_eq!(end, SessionEnd::DisplayLost);
        assert_eq!(log.iter().filter(|l| *l == "reconfigure").count(), 3);
        assert_eq!(log.last().unwrap(), "close keep=false");
    }

    #[tokio::test]
    async fn a_display_that_cannot_be_recreated_ends_the_run() {
        let (log, end, name) = lose_display("lc", 1, true).await;
        assert_eq!(end, SessionEnd::DisplayLost);
        assert_eq!(
            log,
            [
                format!("open {name}").as_str(),
                "start_capture",
                "reconfigure",
                // Dropped on the error path: a real display removes its output.
                "dropped without close"
            ]
        );
    }

    #[tokio::test]
    async fn a_receiver_another_sender_drives_is_left_before_welcome() {
        let (hello, name) = test_hello("lk");
        // The other sender; within one process a second open file is a
        // second lock holder, as another process would be.
        let other = lock::lock_output(&name).unwrap();
        let (log, seen, end) = drive(hello, 0, false).await;
        drop(other);
        forget_lock(&name);
        match end {
            SessionEnd::LockConflict { held, receiver } => {
                assert_eq!(held.name, name);
                assert_eq!(held.pid, Some(std::process::id()));
                assert!(receiver.starts_with("iPhone ("), "{receiver}");
            }
            e => panic!("expected a lock conflict, got {e:?}"),
        }
        assert!(log.is_empty(), "nothing opened: {log:?}");
        assert!(
            !seen.iter().any(|a| matches!(a, ReceiverAction::Welcome(_))),
            "welcome must not be sent: {seen:?}"
        );
    }

    #[test]
    fn a_held_receiver_is_found_before_dialing() {
        let (hello, name) = test_hello("pd");
        let id = hello.id.unwrap();
        let backend = FakeBackend(Shared::default());
        assert_eq!(
            pre_dial_conflict(&backend, &id, Position::AutoRight).unwrap(),
            None
        );
        let other = lock::lock_output(&name).unwrap();
        let held = pre_dial_conflict(&backend, &id, Position::AutoRight)
            .unwrap()
            .expect("conflict");
        assert_eq!(held.name, name);
        drop(other);
        forget_lock(&name);
    }

    #[tokio::test]
    async fn a_panicking_pipeline_thread_reports_a_lost_display() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let h = spawn_reporting_panics("encode", tx, || panic!("injected")).unwrap();
        assert!(h.join().is_err());
        assert_eq!(rx.recv().await.as_deref(), Some("encode thread panicked"));
        // A thread that returns normally reports nothing.
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_reporting_panics("encode", tx, || {})
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn a_panic_in_the_session_still_drops_the_display() {
        let (hello, name) = test_hello("pn");
        let shared = Shared::default();
        shared.panic_on_input.store(true, Ordering::Relaxed);
        let log = shared.clone();
        let r = tokio::spawn(drive_with(shared, hello, 0)).await;
        forget_lock(&name);
        assert!(r.is_err_and(|e| e.is_panic()));
        // Dropping a real display stops capture and then removes the output
        // (wayland::display tests); here: it was dropped, not leaked.
        assert_eq!(
            log.entries(),
            [
                format!("open {name}").as_str(),
                "start_capture",
                "dropped without close"
            ]
        );
    }

    #[test]
    fn receiver_tables_apply_under_the_command_line() {
        let (hello, _) = test_hello("cf");
        let id = hello.id.clone().unwrap();
        let config = Config::parse(&format!(
            "scale = \"mac\"\nbitrate-mbps = 12\n[receiver.\"{id}\"]\nposition = \"auto-left\"\nscale = 2\nbitrate-mbps = 30\n"
        ))
        .unwrap();
        let base = RunOptions {
            cli: Layer {
                bitrate_mbps: Some(8.0),
                ..Layer::default()
            },
            config,
            ..opts()
        };
        let r = base.for_receiver(&[hello.id.as_deref(), Some("test"), Some("iPhone")]);
        assert_eq!(r.position, Position::AutoLeft);
        assert_eq!(r.desktop.scale, crate::desktop::ScalePolicy::Fixed(2.0));
        assert_eq!(r.bitrate_mbps, 8.0, "the command line wins");
        let t = target(&hello, &r).unwrap();
        assert_eq!((t.spec.scale, t.spec.position), (2.0, Position::AutoLeft));
        // Another receiver gets the options as they were.
        let other = base.for_receiver(&[Some("someone-else"), None, None]);
        assert_eq!(other.position, base.position);
    }

    #[test]
    fn the_cursor_goes_separately_only_when_everyone_can() {
        assert!(cursor_separately(CursorMode::Auto, true, true));
        assert!(!cursor_separately(CursorMode::Auto, false, true));
        assert!(!cursor_separately(CursorMode::Auto, true, false));
        assert!(!cursor_separately(CursorMode::Video, true, true));
        assert!(!cursor_separately(CursorMode::Off, true, true));
    }

    /// A receiver offering `cursorPort` gets the cursor as `cursor` and
    /// `cursorImg` messages (mirrored on TCP until a `cursorAck`), and the
    /// video without a painted cursor.
    #[tokio::test]
    async fn the_cursor_rides_the_side_channel_when_offered() {
        let (mut hello, name) = test_hello("cu");
        hello.cursor_port = Some(9001);
        let shared = Shared::default();
        let (sender_io, mut receiver_io) = tokio::io::duplex(1 << 16);
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut backend = FakeBackend(shared.clone());
        let task = tokio::spawn(async move {
            let mut budget = LossBudget::default();
            session(
                sender_io,
                &mut backend,
                &opts(),
                Instant::now(),
                shutdown_rx,
                &mut budget,
                None,
            )
            .await
        });
        let mut rx = ReceiverSession::new(ReceiverConfig::new(hello));
        let rx_now = || Now::new(Duration::from_secs(1), 1e6);
        for a in rx.handle(rx_now(), ReceiverEvent::Connected) {
            if let ReceiverAction::Send(b) = a {
                receiver_io.write_all(&b).await.unwrap();
            }
        }
        let mut buf = vec![0u8; 1 << 16];
        let (mut moved, mut sprite, mut pushed) = (None, None, false);
        while moved.is_none() || sprite.is_none() {
            let n = tokio::time::timeout(Duration::from_secs(5), receiver_io.read(&mut buf))
                .await
                .expect("sender went quiet")
                .unwrap();
            assert!(n > 0, "sender hung up");
            for a in rx.handle(rx_now(), ReceiverEvent::Bytes(&buf[..n])) {
                match a {
                    ReceiverAction::StreamConfig(_) if !pushed => {
                        pushed = true;
                        let mut sink = shared
                            .cursor_sink
                            .lock()
                            .unwrap()
                            .take()
                            .expect("cursor started");
                        sink(CursorEvent::Image {
                            png: b"PNG".to_vec(),
                            nw: 0.02,
                            nh: 0.04,
                            ax: 0.25,
                            ay: 0.5,
                        });
                        sink(CursorEvent::Moved { x: 0.5, y: 0.25 });
                        *shared.cursor_sink.lock().unwrap() = Some(sink);
                    }
                    ReceiverAction::Cursor { x, y, visible } => moved = Some((x, y, visible)),
                    ReceiverAction::CursorImage(img) => sprite = Some(img),
                    _ => {}
                }
            }
        }
        drop(receiver_io);
        task.await.unwrap().unwrap();
        forget_lock(&name);
        assert_eq!(
            *shared.painted.lock().unwrap(),
            Some(false),
            "no cursor in the video"
        );
        assert_eq!(moved, Some((Some(0.5), Some(0.25), true)));
        let img = sprite.unwrap();
        assert_eq!((img.nw, img.nh, img.ax, img.ay), (0.02, 0.04, 0.25, 0.5));
        assert_eq!(img.png, base64(b"PNG"));
        let log = shared.entries();
        assert_eq!(&log[1..3], ["start_cursor", "start_capture"]);
    }

    #[tokio::test]
    async fn without_a_cursor_port_the_cursor_is_painted() {
        let (hello, name) = test_hello("cv");
        let shared = Shared::default();
        let _ = drive_with(shared.clone(), hello, 0).await;
        forget_lock(&name);
        assert_eq!(*shared.painted.lock().unwrap(), Some(true));
        assert!(!shared.entries().iter().any(|l| l == "start_cursor"));
    }

    #[test]
    fn backoff_doubles_to_the_cap_and_resets_after_a_healthy_session() {
        let ms = Duration::from_millis;
        let mut b = Backoff::default();
        let delays: Vec<_> = (0..7).map(|_| b.delay(None)).collect();
        assert_eq!(
            delays,
            [
                ms(500),
                ms(1000),
                ms(2000),
                ms(4000),
                ms(8000),
                ms(10_000),
                ms(10_000)
            ]
        );
        // A short session keeps backing off.
        assert_eq!(b.delay(Some(Duration::from_secs(5))), ms(10_000));
        // One that ran long enough starts over.
        assert_eq!(b.delay(Some(Duration::from_secs(31))), ms(500));
        assert_eq!(b.delay(None), ms(1000));
    }
}
