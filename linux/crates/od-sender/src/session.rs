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
use opendisplay_proto::control::{Hello, StreamConfig};
use opendisplay_session::Now;
use opendisplay_session::sender::{SenderAction, SenderConfig, SenderEvent, SenderSession};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::backend::{
    Backend, CaptureEvent, CaptureOptions, DisplayInfo, DisplaySpec, Frame, FrameSink, InputSink,
    Position, VirtualDisplay,
};
use crate::encoder::{Encoded, Encoder, EncoderSettings};

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub bitrate_mbps: f32,
    /// `None`: follow the receiver's `displayMaxFrameRate`.
    pub max_fps: Option<f32>,
    pub threads: u16,
    pub encoder: crate::encoder::Backend,
    /// Bake the cursor into the video (receivers without cursor rendering).
    pub cursor_in_video: bool,
    /// Where the virtual output goes.
    pub position: Position,
    /// Leave the virtual output in place when the session ends.
    pub keep_output: bool,
    pub reconnect: bool,
    pub duration: Option<Duration>,
}

fn now(start: Instant) -> Now {
    let wall_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    Now::new(start.elapsed(), wall_ms)
}

fn short_id(hello: &Hello) -> String {
    hello
        .id
        .as_deref()
        .map(|s| s.chars().take(6).collect::<String>().to_lowercase())
        .unwrap_or_else(|| "anon".into())
}

/// The submission-rate ceiling: the receiver's panel rate, lowered by
/// `--max-fps`. A 120 Hz panel fed 60 fps waits up to a whole extra
/// interval per frame, which is felt most in pointer motion.
fn stream_fps(opts: &RunOptions, hello: &Hello) -> u32 {
    let panel = hello.display_max_frame_rate.unwrap_or(60).clamp(30, 240);
    opts.max_fps
        .map_or(panel, |cap| (cap.round().max(1.0) as u32).min(panel))
}

/// What to build for a `hello`.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopPlan {
    pub display: DisplaySpec,
    pub frames_per_second: u32,
}

/// Seam for `desktop::plan(&Hello, &Opts)` (SENDER-PLAN §3.6, being written
/// separately). Today's behaviour: the output takes `hello`'s pixels and
/// scale verbatim, at the panel rate.
pub fn desktop_plan(hello: &Hello, opts: &RunOptions) -> DesktopPlan {
    DesktopPlan {
        display: DisplaySpec {
            name: format!("od-{}", short_id(hello)),
            width: hello.pixels_wide,
            height: hello.pixels_high,
            hz: hello.display_max_frame_rate.unwrap_or(60).clamp(30, 240),
            scale: if hello.scale >= 1.0 { hello.scale } else { 1.0 },
            position: opts.position,
        },
        frames_per_second: stream_fps(opts, hello),
    }
}

/// A repeated `hello` rebuilds the display only when the output would change.
fn needs_rebuild(current: Option<&DisplaySpec>, next: &DisplaySpec) -> bool {
    current.is_none_or(|c| {
        c.name != next.name
            || c.width != next.width
            || c.height != next.height
            || c.scale != next.scale
    })
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

/// Encoder thread fed by a capture [`FrameSink`]: latest frame wins, rate
/// capped, last frame replayed as an IDR on a static screen (§5.3).
struct EncodePipeline {
    stop: Arc<AtomicBool>,
    force_idr: Arc<AtomicBool>,
    raw_tx: Option<std::sync::mpsc::SyncSender<Frame>>,
    encoded_rx: mpsc::Receiver<Encoded>,
    lost_tx: mpsc::UnboundedSender<String>,
    lost_rx: mpsc::UnboundedReceiver<String>,
}

impl EncodePipeline {
    fn start(mut settings: EncoderSettings) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let force_idr = Arc::new(AtomicBool::new(true));
        let (raw_tx, raw_rx) = std::sync::mpsc::sync_channel::<Frame>(1);
        let (encoded_tx, encoded_rx) = mpsc::channel::<Encoded>(4);
        let (lost_tx, lost_rx) = mpsc::unbounded_channel();
        let enc_stop = stop.clone();
        let enc_force = force_idr.clone();
        std::thread::Builder::new()
            .name("encode".into())
            .spawn(move || {
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

    /// The capture side: drops a frame when the encoder is busy, reports
    /// the end of capture to the session.
    fn sink(&mut self) -> FrameSink {
        let raw_tx = self.raw_tx.take().expect("one sink per pipeline");
        let lost_tx = self.lost_tx.clone();
        Box::new(move |ev| match ev {
            CaptureEvent::Frame(f) => {
                let _ = raw_tx.try_send(f);
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
    plan: DesktopPlan,
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
    plan: DesktopPlan,
    opts: &RunOptions,
) -> Result<DisplayInfo> {
    let mut display = match active.take() {
        Some(Active {
            input,
            mut display,
            pipe,
            plan: old,
        }) => {
            drop(input);
            display.stop_capture();
            drop(pipe);
            if old.display.name == plan.display.name {
                display.reconfigure(&plan.display)?;
                display
            } else {
                if let Err(e) = display.close(opts.keep_output) {
                    warn!("closing {}: {e:#}", old.display.name);
                }
                backend.open(&plan.display)?
            }
        }
        None => backend.open(&plan.display)?,
    };
    let mut pipe = EncodePipeline::start(EncoderSettings {
        backend: opts.encoder,
        bitrate_bps: (opts.bitrate_mbps * 1e6) as u32,
        max_fps: plan.frames_per_second as f32,
        threads: opts.threads,
    })?;
    display.start_capture(
        CaptureOptions {
            cursor: opts.cursor_in_video,
        },
        pipe.sink(),
    )?;
    let input = match display.input() {
        Ok(i) => Some(i),
        Err(e) => {
            warn!("input injection unavailable: {e:#}");
            None
        }
    };
    let info = display.info().clone();
    *active = Some(Active {
        input,
        display,
        pipe,
        plan,
    });
    Ok(info)
}

/// Resolves to `true` on SIGINT or SIGTERM. Handled explicitly so the output
/// is torn down in order, and because a sender started in the background by
/// a non-interactive shell inherits SIGINT as ignored.
fn shutdown_signal() -> Result<watch::Receiver<bool>> {
    use tokio::signal::unix::{SignalKind, signal};
    let (tx, rx) = watch::channel(false);
    let mut int = signal(SignalKind::interrupt())?;
    let mut term = signal(SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = int.recv() => {}
            _ = term.recv() => {}
        }
        info!("shutting down");
        let _ = tx.send(true);
    });
    Ok(rx)
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// Connection dropped or shutdown; redial if allowed.
    Ended,
    /// The receiver said `closing`: do not redial.
    ReceiverClosed,
    /// The display kept vanishing or could not be recreated: stop.
    DisplayLost,
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
    loop {
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
                match session(stream, backend, &opts, start, shutdown.clone(), &mut losses).await {
                    Ok(SessionEnd::ReceiverClosed) => {
                        info!("receiver closed the session for good");
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
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
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
    info: &DisplayInfo,
    fps: u32,
) -> Result<()> {
    let cfg = sess.stream_config(StreamConfig {
        codec: "h264".into(),
        width: info.width & !1,
        height: info.height & !1,
        frames_per_second: fps,
    });
    if let SenderAction::Send(b) = cfg {
        stream.write_all(&b).await.context("write")?;
    }
    Ok(())
}

async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    backend: &mut dyn Backend,
    opts: &RunOptions,
    start: Instant,
    mut shutdown: watch::Receiver<bool>,
    losses: &mut LossBudget,
) -> Result<SessionEnd> {
    let mut sess = SenderSession::new(SenderConfig::default());
    sess.handle(now(start), SenderEvent::Connected);
    let mut active: Option<Active> = None;
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
                        let plan = active.as_ref().map(|a| a.plan.clone()).expect("lost implies active");
                        warn!("virtual display {} lost: {why}", plan.display.name);
                        if !losses.allow(Instant::now()) {
                            warn!("{} lost too often; closing the connection", plan.display.name);
                            return Ok(SessionEnd::DisplayLost);
                        }
                        let fps = plan.frames_per_second;
                        match build(backend, &mut active, plan, opts) {
                            Ok(info) => {
                                info!("recreated virtual display {}", info.name);
                                send_stream_config(&mut stream, &sess, &info, fps).await?;
                            }
                            Err(e) => {
                                warn!("cannot recreate the virtual display: {e:#}; closing the connection");
                                return Ok(SessionEnd::DisplayLost);
                            }
                        }
                        vec![]
                    }
                },
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
                    SenderAction::SendCursorDatagram(_) => {}
                    SenderAction::Hello(h) => {
                        info!(
                            "hello: {}x{} @{} scale {} device {:?} id {:?} pv {}",
                            h.pixels_wide, h.pixels_high, h.display_max_frame_rate.unwrap_or(60), h.scale, h.device, h.id, sess.receiver_pv()
                        );
                        let plan = desktop_plan(&h, opts);
                        if needs_rebuild(active.as_ref().map(|a| &a.plan.display), &plan.display) {
                            let fps = plan.frames_per_second;
                            let info = build(backend, &mut active, plan, opts)?;
                            send_stream_config(&mut stream, &sess, &info, fps).await?;
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
            max_fps: None,
            threads: 1,
            encoder: crate::encoder::Backend::Auto,
            cursor_in_video: true,
            position: Position::AutoRight,
            keep_output: false,
            reconnect: false,
            duration: None,
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
    fn plan_keeps_todays_behaviour() {
        let p = desktop_plan(&hello(), &opts());
        assert_eq!(p.display.name, "od-3f9a1c");
        assert_eq!((p.display.width, p.display.height), (2532, 1170));
        assert_eq!((p.display.hz, p.display.scale), (120, 3.0));
        assert_eq!(p.frames_per_second, 120);
        let capped = desktop_plan(
            &hello(),
            &RunOptions {
                max_fps: Some(30.0),
                ..opts()
            },
        );
        assert_eq!(capped.frames_per_second, 30);
        let anon = desktop_plan(
            &Hello {
                id: None,
                scale: 0.0,
                display_max_frame_rate: None,
                ..hello()
            },
            &opts(),
        );
        assert_eq!(anon.display.name, "od-anon");
        assert_eq!((anon.display.hz, anon.display.scale), (60, 1.0));
    }

    #[test]
    fn rebuild_only_when_the_output_changes() {
        let a = desktop_plan(&hello(), &opts()).display;
        assert!(needs_rebuild(None, &a));
        assert!(!needs_rebuild(Some(&a), &a.clone()));
        let rotated = DisplaySpec {
            width: a.height,
            height: a.width,
            ..a.clone()
        };
        assert!(needs_rebuild(Some(&a), &rotated));
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
    struct FakeDisplay(Shared, DisplayInfo);
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
                cursor_metadata: false,
                dmabuf: false,
                resize_in_place: true,
                creates_outputs: true,
            }
        }
        fn open(&mut self, spec: &DisplaySpec) -> Result<Box<dyn VirtualDisplay>> {
            self.0.log(format!("open {}", spec.name));
            Ok(Box::new(FakeDisplay(self.0.clone(), info_of(spec))))
        }
        fn clean(&mut self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    impl VirtualDisplay for FakeDisplay {
        fn info(&self) -> &DisplayInfo {
            &self.1
        }
        fn start_capture(&mut self, _: CaptureOptions, sink: FrameSink) -> Result<()> {
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
            Ok(Box::new(FakeInput))
        }
        fn close(mut self: Box<Self>, keep_output: bool) -> Result<()> {
            self.stop_capture();
            self.0.log(format!("close keep={keep_output}"));
            Ok(())
        }
    }

    /// Drive one session against a `ReceiverSession` over an in-memory pipe.
    /// After each `streamConfig`, capture ends like an output removed under
    /// it, `losses` times; then the receiver hangs up. Returns the backend
    /// log and how the session ended.
    async fn lose_display(losses: usize, fail_reconfigure: bool) -> (Vec<String>, SessionEnd) {
        let shared = Shared::default();
        shared
            .fail_reconfigure
            .store(fail_reconfigure, Ordering::Relaxed);
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
            )
            .await
        });

        let mut rx = ReceiverSession::new(ReceiverConfig::new(hello()));
        let rx_now = || Now::new(Duration::from_secs(1), 1e6);
        for a in rx.handle(rx_now(), ReceiverEvent::Connected) {
            if let ReceiverAction::Send(b) = a {
                receiver_io.write_all(&b).await.unwrap();
            }
        }
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
                if !matches!(a, ReceiverAction::StreamConfig(_)) {
                    continue;
                }
                if lost == losses {
                    drop(receiver_io);
                    let end = session_task.await.unwrap().unwrap();
                    return (shared.entries(), end);
                }
                lost += 1;
                let mut sink = shared.sink.lock().unwrap().take().expect("capturing");
                sink(CaptureEvent::Ended("output removed".into()));
            }
        }
        let end = session_task.await.unwrap().unwrap();
        (shared.entries(), end)
    }

    #[tokio::test]
    async fn a_lost_display_is_recreated_from_the_last_plan() {
        let (log, end) = lose_display(1, false).await;
        assert_eq!(end, SessionEnd::Ended);
        assert_eq!(
            log,
            [
                "open od-3f9a1c",
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
        let (log, end) = lose_display(4, false).await;
        assert_eq!(end, SessionEnd::DisplayLost);
        assert_eq!(log.iter().filter(|l| *l == "reconfigure").count(), 3);
        assert_eq!(log.last().unwrap(), "close keep=false");
    }

    #[tokio::test]
    async fn a_display_that_cannot_be_recreated_ends_the_run() {
        let (log, end) = lose_display(1, true).await;
        assert_eq!(end, SessionEnd::DisplayLost);
        assert_eq!(log, ["open od-3f9a1c", "start_capture", "reconfigure"]);
    }
}
