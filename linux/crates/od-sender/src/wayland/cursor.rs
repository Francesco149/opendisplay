//! The pointer cursor of one output, delivered on its own for the cursor
//! side channel (PROTOCOL §6.2 `cursor`/`cursorImg`, §6.3), through an
//! `ext_image_copy_capture_cursor_session_v1`: `enter`/`leave`/`position`
//! events for where the hotspot is, and an image capture session for the
//! sprite. Runs on its own thread with its own Wayland connection.
//!
//! The cursor session is a capture of the output like the frame capture, so
//! the same rule holds: it is stopped and joined before the output is
//! reconfigured or removed ([`super::display::CapturedDisplay`] does both).

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};
use wayland_client::protocol::{
    wl_buffer, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1, ext_output_image_capture_source_manager_v1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_cursor_session_v1, ext_image_copy_capture_frame_v1,
    ext_image_copy_capture_manager_v1, ext_image_copy_capture_session_v1,
};

use crate::backend::{CursorEvent, CursorSink};

/// The official sender's ceiling for position updates (PROTOCOL §6.2).
const MIN_MOVE_INTERVAL: Duration = Duration::from_micros(8_333);
/// `cursorImg` PNGs stay under this many bytes before base64 (PROTOCOL §6.2).
pub const MAX_PNG_BYTES: usize = 24_000;

// ---------------------------------------------------------------------------
// Sprite encoding: premultiplied BGRA (wl_shm ARGB8888) -> PNG -> base64.

/// Straight-alpha RGBA from little-endian premultiplied ARGB8888 (B, G, R, A).
pub fn bgra_premul_to_rgba(bgra: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        let row = &bgra[y * stride as usize..][..width as usize * 4];
        for px in row.chunks_exact(4) {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            let un = |c: u8| {
                if a == 0 {
                    0
                } else {
                    ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                }
            };
            out.extend_from_slice(&[un(r), un(g), un(b), a]);
        }
    }
    out
}

/// Halve an RGBA sprite (2x2 box, alpha-weighted so edges do not darken).
pub fn halve_rgba(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let (w2, h2) = ((width / 2).max(1), (height / 2).max(1));
    let mut out = Vec::with_capacity((w2 * h2 * 4) as usize);
    for y in 0..h2 {
        for x in 0..w2 {
            let mut acc = [0u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = ((2 * x + dx).min(width - 1), (2 * y + dy).min(height - 1));
                let p = &rgba[((sy * width + sx) * 4) as usize..][..4];
                let a = p[3] as u32;
                for c in 0..3 {
                    acc[c] += p[c] as u32 * a;
                }
                acc[3] += a;
            }
            let a = acc[3];
            let px = |c: usize| (acc[c] + a / 2).checked_div(a).unwrap_or(0) as u8;
            out.extend_from_slice(&[px(0), px(1), px(2), ((a + 2) / 4) as u8]);
        }
    }
    (out, w2, h2)
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for data in chunks {
        for &b in *data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
    }
    !crc
}

/// An 8-bit RGBA PNG.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width * height * 4 + height) as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 9);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bit, RGBA, deflate, adaptive, no interlace
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (b"IHDR", &ihdr[..]),
        (b"IDAT", &idat[..]),
        (b"IEND", &[][..]),
    ] {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        png.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    }
    png
}

/// The sprite as a PNG under [`MAX_PNG_BYTES`], halving it while it is not.
pub fn sprite_png(mut rgba: Vec<u8>, mut width: u32, mut height: u32) -> Vec<u8> {
    loop {
        let png = encode_png(&rgba, width, height);
        if png.len() <= MAX_PNG_BYTES || (width <= 8 && height <= 8) {
            return png;
        }
        (rgba, width, height) = halve_rgba(&rgba, width, height);
    }
}

/// Standard base64 with padding (`cursorImg.png`).
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Position: the hotspot in output pixels -> normalized, rate-limited updates.

/// Turns `enter`/`leave`/`position` into [`CursorEvent`]s: positions are
/// normalized to the output, coalesced to [`MIN_MOVE_INTERVAL`], and
/// repeated positions dropped.
pub struct Positions {
    width: f64,
    height: f64,
    inside: bool,
    hidden_sent: bool,
    latest: Option<(i32, i32)>,
    sent: Option<(i32, i32)>,
    last_emit: Option<Instant>,
}

impl Positions {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(1) as f64,
            height: height.max(1) as f64,
            inside: false,
            hidden_sent: false,
            latest: None,
            sent: None,
            last_emit: None,
        }
    }

    pub fn enter(&mut self) {
        self.inside = true;
    }

    pub fn leave(&mut self) {
        self.inside = false;
        self.latest = None;
        self.sent = None;
    }

    pub fn position(&mut self, x: i32, y: i32) {
        self.latest = Some((x, y));
    }

    /// The event to send now, if any, and when to ask again.
    pub fn poll(&mut self, now: Instant) -> (Option<CursorEvent>, Option<Duration>) {
        if !self.inside {
            if self.hidden_sent {
                return (None, None);
            }
            self.hidden_sent = true;
            return (Some(CursorEvent::Hidden), None);
        }
        let Some(p) = self.latest else {
            return (None, None);
        };
        if self.sent == Some(p) {
            return (None, None);
        }
        if let Some(t) = self.last_emit {
            let since = now.saturating_duration_since(t);
            if since < MIN_MOVE_INTERVAL {
                return (None, Some(MIN_MOVE_INTERVAL - since));
            }
        }
        self.sent = Some(p);
        self.hidden_sent = false;
        self.last_emit = Some(now);
        let ev = CursorEvent::Moved {
            x: p.0 as f64 / self.width,
            y: p.1 as f64 / self.height,
        };
        (Some(ev), None)
    }
}

// ---------------------------------------------------------------------------
// The Wayland side.

struct ShmBuffer {
    file: std::fs::File,
    _pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    stride: u32,
}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        width: u32,
        height: u32,
        format: wl_shm::Format,
    ) -> Result<Self> {
        let stride = width * 4;
        let size = (stride * height) as u64;
        let path = format!(
            "/dev/shm/od-cursor-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        let _ = std::fs::remove_file(&path);
        file.set_len(size)?;
        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            qh,
            (),
        );
        Ok(Self {
            file,
            _pool: pool,
            buffer,
            width,
            height,
            stride,
        })
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

#[derive(Default)]
struct State {
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    outputs: HashMap<u32, (Option<wl_output::WlOutput>, String)>,
    source_mgr:
        Option<ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1>,
    capture_mgr: Option<ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1>,
    // Image session constraints.
    size: (u32, u32),
    formats: Vec<wl_shm::Format>,
    constraints_done: bool,
    stopped: bool,
    // Cursor session.
    entered: Option<bool>,
    positions: Vec<(i32, i32)>,
    hotspot_pending: Option<(i32, i32)>,
    // Frame.
    ready: bool,
    failed: Option<WEnum<ext_image_copy_capture_frame_v1::FailureReason>>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        st: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_shm" => st.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" if st.seat.is_none() => {
                    st.seat = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "wl_output" if version >= 4 => {
                    let o: wl_output::WlOutput = registry.bind(name, 4, qh, name);
                    st.outputs.insert(name, (Some(o), String::new()));
                }
                "ext_output_image_capture_source_manager_v1" => {
                    st.source_mgr = Some(registry.bind(name, 1, qh, ()))
                }
                "ext_image_copy_capture_manager_v1" => {
                    st.capture_mgr = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        st: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some(o) = st.outputs.get_mut(global) {
                o.1 = name;
            }
        }
    }
}

impl Dispatch<ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        st: &mut Self,
        _: &ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        match event {
            Event::BufferSize { width, height } => {
                st.size = (width, height);
                st.formats.clear();
            }
            Event::ShmFormat {
                format: WEnum::Value(f),
            } => st.formats.push(f),
            Event::Done => st.constraints_done = true,
            Event::Stopped => st.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1, ()>
    for State
{
    fn event(
        st: &mut Self,
        _: &ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1,
        event: ext_image_copy_capture_cursor_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_cursor_session_v1::Event;
        match event {
            Event::Enter => st.entered = Some(true),
            Event::Leave => st.entered = Some(false),
            Event::Position { x, y } => st.positions.push((x, y)),
            Event::Hotspot { x, y } => st.hotspot_pending = Some((x, y)),
            _ => {}
        }
    }
}

impl Dispatch<ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        st: &mut Self,
        _: &ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        match event {
            Event::Ready => st.ready = true,
            Event::Failed { reason } => st.failed = Some(reason),
            _ => {}
        }
    }
}

macro_rules! noop_dispatch {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
noop_dispatch!(
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_seat::WlSeat,
    wl_pointer::WlPointer,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1
);

/// The cursor of one output on its own thread. [`stop`](Self::stop) (or
/// drop) joins it, so once it returns the cursor session is gone.
pub struct CursorThread {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CursorThread {
    /// Start delivering the cursor of output `name`, `width`x`height` pixels.
    /// Returns once the cursor session exists, or with the reason it cannot.
    pub fn spawn(name: &str, width: u32, height: u32, sink: CursorSink) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let name = name.to_string();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let handle = std::thread::Builder::new()
            .name("cursor".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&name, width, height, sink, &thread_stop, &ready_tx)
                }));
                match r {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        // Before the session existed this goes to `spawn`'s caller.
                        if ready_tx.send(Err(anyhow::anyhow!("{e:#}"))).is_err() {
                            warn!("cursor capture of {name} ended: {e:#}");
                        }
                    }
                    Err(_) => warn!("cursor thread panicked; the cursor stays where it was"),
                }
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                handle: Some(handle),
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                bail!("cursor thread died")
            }
        }
    }

    /// Stop and join. Idempotent.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for CursorThread {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pick_format(formats: &[wl_shm::Format]) -> Option<wl_shm::Format> {
    // The sprite needs alpha; ARGB8888 is what every compositor offers.
    [wl_shm::Format::Argb8888]
        .into_iter()
        .find(|f| formats.contains(f))
}

fn run(
    name: &str,
    width: u32,
    height: u32,
    mut sink: CursorSink,
    stop: &AtomicBool,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut st = State::default();
    queue.roundtrip(&mut st)?;
    queue.roundtrip(&mut st)?;
    let shm = st.shm.clone().context("compositor has no wl_shm")?;
    let seat = st.seat.clone().context("compositor has no wl_seat")?;
    let source_mgr = st
        .source_mgr
        .clone()
        .context("compositor lacks ext_output_image_capture_source_manager_v1")?;
    let capture_mgr = st
        .capture_mgr
        .clone()
        .context("compositor lacks ext_image_copy_capture_manager_v1")?;
    let output = st
        .outputs
        .values()
        .find(|(_, n)| n == name)
        .and_then(|(o, _)| o.clone())
        .with_context(|| format!("no output {name}"))?;

    let pointer = seat.get_pointer(&qh, ());
    let source = source_mgr.create_source(&output, &qh, ());
    let cursor_session = capture_mgr.create_pointer_cursor_session(&source, &pointer, &qh, ());
    let session = cursor_session.get_capture_session(&qh, ());
    queue.roundtrip(&mut st)?;
    if st.stopped {
        bail!("the compositor refused the cursor session");
    }
    let _ = ready.send(Ok(()));
    info!("cursor of {name} goes to the side channel");

    let mut positions = Positions::new(width, height);
    let mut buffer: Option<ShmBuffer> = None;
    let mut frame: Option<ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1> = None;
    let mut hotspot = (0, 0);
    let mut last_sprite: Option<(Vec<u8>, (i32, i32))> = None;
    let mut wait = Duration::from_millis(100);
    while !stop.load(Ordering::Relaxed) && !st.stopped {
        dispatch_with_timeout(&conn, &mut queue, &mut st, wait)?;
        wait = Duration::from_millis(100);

        match st.entered.take() {
            Some(true) => positions.enter(),
            Some(false) => positions.leave(),
            None => {}
        }
        for (x, y) in st.positions.drain(..) {
            positions.position(x, y);
        }
        let (ev, again) = positions.poll(Instant::now());
        if let Some(ev) = ev {
            sink(ev);
        }
        if let Some(again) = again {
            wait = again;
        }

        // The sprite: capture a frame whenever the constraints are known; the
        // compositor completes it when the cursor image changes.
        if st.constraints_done && st.size.0 > 0 && st.size.1 > 0 {
            let fits = buffer
                .as_ref()
                .is_some_and(|b| (b.width, b.height) == st.size);
            if !fits {
                if let Some(f) = frame.take() {
                    f.destroy();
                }
                buffer = match pick_format(&st.formats) {
                    Some(fmt) => Some(ShmBuffer::new(&shm, &qh, st.size.0, st.size.1, fmt)?),
                    None => {
                        warn!("cursor session offers no ARGB8888; no sprite");
                        None
                    }
                };
            }
        }
        if st.ready {
            st.ready = false;
            if let Some(f) = frame.take() {
                f.destroy();
            }
            if let Some(h) = st.hotspot_pending.take() {
                hotspot = h;
            }
            if let Some(b) = &buffer {
                let mut px = vec![0u8; (b.stride * b.height) as usize];
                b.file.read_exact_at(&mut px, 0)?;
                if last_sprite.as_ref() != Some(&(px.clone(), hotspot)) {
                    let rgba = bgra_premul_to_rgba(&px, b.width, b.height, b.stride);
                    let png = sprite_png(rgba, b.width, b.height);
                    debug!(
                        "cursor sprite {}x{} hotspot {hotspot:?}: {} bytes PNG",
                        b.width,
                        b.height,
                        png.len()
                    );
                    sink(CursorEvent::Image {
                        png,
                        nw: b.width as f64 / width.max(1) as f64,
                        nh: b.height as f64 / height.max(1) as f64,
                        ax: hotspot.0 as f64 / b.width as f64,
                        ay: hotspot.1 as f64 / b.height as f64,
                    });
                    last_sprite = Some((px, hotspot));
                }
            }
        } else if let Some(reason) = st.failed.take() {
            if let Some(f) = frame.take() {
                f.destroy();
            }
            match reason {
                WEnum::Value(ext_image_copy_capture_frame_v1::FailureReason::Stopped) => break,
                WEnum::Value(ext_image_copy_capture_frame_v1::FailureReason::BufferConstraints) => {
                    buffer = None;
                }
                _ => debug!("cursor frame failed: {reason:?}"),
            }
        }
        if frame.is_none() {
            if let Some(b) = &buffer {
                let f = session.create_frame(&qh, ());
                f.attach_buffer(&b.buffer);
                f.damage_buffer(0, 0, b.width as i32, b.height as i32);
                f.capture();
                frame = Some(f);
            }
        }
    }
    if let Some(f) = frame.take() {
        f.destroy();
    }
    drop(buffer);
    session.destroy();
    cursor_session.destroy();
    source.destroy();
    if pointer.version() >= 3 {
        pointer.release();
    }
    let _ = queue.roundtrip(&mut st);
    debug!("cursor thread done");
    Ok(())
}

/// `blocking_dispatch` with an upper bound, so the thread notices `stop`.
fn dispatch_with_timeout(
    conn: &Connection,
    queue: &mut wayland_client::EventQueue<State>,
    st: &mut State,
    timeout: Duration,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    queue.dispatch_pending(st)?;
    conn.flush()?;
    let Some(guard) = conn.prepare_read() else {
        queue.dispatch_pending(st)?;
        return Ok(());
    };
    let mut pfd = PollFd {
        fd: guard.connection_fd().as_raw_fd(),
        events: 0x001, // POLLIN
        revents: 0,
    };
    // SAFETY: one valid pollfd for the duration of the call.
    let r = unsafe { poll(&mut pfd, 1, timeout.as_millis() as i32) };
    if r > 0 {
        let _ = guard.read();
        queue.dispatch_pending(st)?;
    }
    Ok(())
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn premultiplied_bgra_becomes_straight_rgba() {
        // Opaque red, half-transparent white (premultiplied 128), transparent.
        let bgra = [0, 0, 255, 255, 128, 128, 128, 128, 0, 0, 0, 0, 9, 9, 9, 9];
        let rgba = bgra_premul_to_rgba(&bgra, 3, 1, 16);
        assert_eq!(rgba, [255, 0, 0, 255, 255, 255, 255, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn png_is_well_formed_and_round_trips() {
        let (w, h) = (5u32, 3u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7) as u8).collect();
        let png = encode_png(&rgba, w, h);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // Walk the chunks, check each CRC, and inflate IDAT.
        let mut at = 8;
        let mut kinds = Vec::new();
        let mut idat = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let data = &png[at + 8..at + 8 + len];
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&[kind, data]));
            if kind == b"IDAT" {
                idat.extend_from_slice(data);
            }
            kinds.push(String::from_utf8(kind.to_vec()).unwrap());
            at += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).unwrap();
        let rows: Vec<u8> = raw
            .chunks_exact(w as usize * 4 + 1)
            .flat_map(|r| {
                assert_eq!(r[0], 0);
                r[1..].to_vec()
            })
            .collect();
        assert_eq!(rows, rgba);
        // The well-known CRC of "IEND".
        assert_eq!(crc32(&[b"IEND"]), 0xae42_6082);
    }

    #[test]
    fn big_noisy_sprites_shrink_under_the_limit() {
        let (w, h) = (256u32, 256u32);
        let mut x = 0x1234_5678u32;
        let rgba: Vec<u8> = (0..w * h * 4)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        assert!(encode_png(&rgba, w, h).len() > MAX_PNG_BYTES);
        assert!(sprite_png(rgba, w, h).len() <= MAX_PNG_BYTES);
    }

    #[test]
    fn halving_weights_by_alpha() {
        // One opaque white pixel next to three transparent black ones.
        let rgba = [255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let (out, w, h) = halve_rgba(&rgba, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, [255, 255, 255, 64]);
    }

    #[test]
    fn positions_normalize_coalesce_and_hide() {
        let t = Instant::now();
        let mut p = Positions::new(2000, 1000);
        // Starts hidden, says so once.
        assert_eq!(p.poll(t).0, Some(CursorEvent::Hidden));
        assert_eq!(p.poll(t).0, None);
        p.enter();
        p.position(500, 250);
        assert_eq!(p.poll(t).0, Some(CursorEvent::Moved { x: 0.25, y: 0.25 }));
        // Within the rate cap: held, with a retry time; the newest wins.
        p.position(600, 250);
        p.position(1000, 500);
        let (ev, again) = p.poll(t + Duration::from_millis(2));
        assert_eq!(ev, None);
        assert!(again.unwrap() <= MIN_MOVE_INTERVAL);
        assert_eq!(
            p.poll(t + Duration::from_millis(10)).0,
            Some(CursorEvent::Moved { x: 0.5, y: 0.5 })
        );
        // No movement, nothing sent.
        assert_eq!(p.poll(t + Duration::from_millis(30)).0, None);
        p.leave();
        assert_eq!(
            p.poll(t + Duration::from_millis(40)).0,
            Some(CursorEvent::Hidden)
        );
        // Re-entering at the same spot sends it again.
        p.enter();
        p.position(1000, 500);
        assert_eq!(
            p.poll(t + Duration::from_millis(50)).0,
            Some(CursorEvent::Moved { x: 0.5, y: 0.5 })
        );
    }
}
