//! What differs per compositor: creating a virtual output, getting its
//! frames, and injecting input (SENDER-PLAN §3.2). A backend is chosen once at
//! startup by [`detect`]; the session loop only sees the traits below.

pub mod adopt;
pub mod detect;
pub mod hyprland;
pub mod lock;

use std::fmt;
use std::str::FromStr;
use std::time::Instant;

use anyhow::{Result, bail};
use clap::ValueEnum;
use opendisplay_proto::control::{Pencil, Scroll, Touch};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendId {
    /// Hyprland >= 0.55: headless outputs over the IPC socket.
    Hyprland,
    /// An output that already exists (`--output <name>`): capture and input
    /// only, nothing is created or removed.
    Adopt,
    /// Sway and other wlroots compositors (not implemented yet).
    Wlroots,
    /// KDE Plasma (not implemented yet).
    Kde,
    /// xdg-desktop-portal ScreenCast/RemoteDesktop, e.g. GNOME (not implemented yet).
    Portal,
}

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BackendId::Hyprland => "hyprland",
            BackendId::Adopt => "adopt",
            BackendId::Wlroots => "wlroots",
            BackendId::Kde => "kde",
            BackendId::Portal => "portal",
        })
    }
}

/// Where the virtual output goes relative to the existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    AutoRight,
    AutoLeft,
    AutoUp,
    AutoDown,
    At(i32, i32),
}

impl FromStr for Position {
    type Err = anyhow::Error;

    /// `auto-right|auto-left|auto-up|auto-down`, or `x,y` / `XxY`.
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "auto-right" => Position::AutoRight,
            "auto-left" => Position::AutoLeft,
            "auto-up" => Position::AutoUp,
            "auto-down" => Position::AutoDown,
            _ => {
                let Some((x, y)) = s.split_once(',').or_else(|| s.split_once('x')) else {
                    bail!(
                        "position {s:?}: expected auto-right, auto-left, auto-up, auto-down or x,y"
                    );
                };
                At(x.trim().parse()?, y.trim().parse()?)
            }
        })
    }
}
use Position::At;

/// What the session asks for, derived from `hello` (SENDER-PLAN §3.6).
#[derive(Debug, Clone, PartialEq)]
pub struct DisplaySpec {
    /// `od-<6 chars of hello.id>`; also the lock key.
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub scale: f64,
    pub position: Position,
}

/// The display as the compositor actually configured it.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub scale: f64,
    pub x: i32,
    pub y: i32,
}

impl fmt::Display for DisplayInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} = {}x{} @{:.0} scale {} at {},{} ({}x{} logical)",
            self.name,
            self.width,
            self.height,
            self.refresh_hz,
            self.scale,
            self.x,
            self.y,
            (self.width as f64 / self.scale).round(),
            (self.height as f64 / self.scale).round()
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Cursor delivered separately (ext cursor session / portal METADATA).
    pub cursor_metadata: bool,
    /// Capture can hand out GPU buffers.
    pub dmabuf: bool,
    /// Reconfigure without destroy+create.
    pub resize_in_place: bool,
    /// False for Adopt.
    pub creates_outputs: bool,
}

impl fmt::Display for Caps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flag = |b: bool| if b { "yes" } else { "no" };
        write!(
            f,
            "creates outputs {}, resize in place {}, cursor metadata {}, dmabuf {}",
            flag(self.creates_outputs),
            flag(self.resize_in_place),
            flag(self.cursor_metadata),
            flag(self.dmabuf)
        )
    }
}

pub trait Backend: Send {
    fn id(&self) -> BackendId;
    /// One line for logs and `doctor`.
    fn describe(&self) -> String;
    fn caps(&self) -> Caps;
    /// Create (or adopt an unlocked leftover) and size the virtual output.
    fn open(&mut self, spec: &DisplaySpec) -> Result<Box<dyn VirtualDisplay>>;
    /// Remove `od-*` outputs that no running sender holds a lock on.
    fn clean(&mut self) -> Result<Vec<String>>;
    /// The output lock [`open`](Self::open) takes for `spec`.
    fn lock_name(&self, spec: &DisplaySpec) -> String {
        spec.name.clone()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    /// Paint the cursor into the frames.
    pub cursor: bool,
}

/// One virtual display. Capture always stops before the output is
/// reconfigured or removed, including on drop: Hyprland 0.56.2 aborts in
/// `CScreenshareFrame::transform` when a monitor changes under a frame in
/// flight.
pub trait VirtualDisplay: Send {
    fn info(&self) -> &DisplayInfo;
    /// Start capturing on a thread the display owns; frames go to `sink`.
    /// If capture ends without [`stop_capture`](Self::stop_capture) (the
    /// output was removed, the compositor stopped the session) the sink gets
    /// one [`CaptureEvent::Ended`] as its last event.
    fn start_capture(&mut self, opts: CaptureOptions, sink: FrameSink) -> Result<()>;
    /// Deliver the pointer cursor on its own (position and sprite, for the
    /// cursor side channel, PROTOCOL §6.3) instead of painting it into the
    /// frames. Call before [`start_capture`](Self::start_capture) with
    /// `cursor: false`; [`stop_capture`](Self::stop_capture) stops it too.
    /// Errs when the compositor cannot; then paint the cursor instead.
    fn start_cursor(&mut self, _sink: CursorSink) -> Result<()> {
        bail!("this display cannot deliver the cursor separately")
    }
    /// Stop capture (frames and cursor) and join its threads. Idempotent.
    fn stop_capture(&mut self);
    /// Resize in place; stops capture first.
    fn reconfigure(&mut self, spec: &DisplaySpec) -> Result<()>;
    /// Input bound to this display. Call again after `reconfigure`.
    fn input(&mut self) -> Result<Box<dyn InputSink>>;
    /// Stop capture, then remove the output unless `keep_output`.
    fn close(self: Box<Self>, keep_output: bool) -> Result<()>;
}

/// In-memory layout of a CPU frame, named as `wl_shm`/DRM fourccs are
/// (little-endian: `Xrgb8888` is B, G, R, X in memory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Xrgb8888,
    Argb8888,
    Xbgr8888,
    Abgr8888,
}

pub struct Frame {
    /// When the compositor reported the frame ready, before it is copied out.
    pub captured_at: Instant,
    /// Compositor presentation time, if reported.
    #[allow(dead_code)] // for the cursor side channel and latency stats
    pub presentation_ns: Option<u128>,
    pub buf: FrameBuf,
}

pub enum FrameBuf {
    Cpu {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
        #[allow(dead_code)] // the encoders assume BGRX/BGRA, which is what capture prefers
        format: PixelFormat,
    },
    /// GPU buffer for zero-copy hardware encode; not produced yet (VA-API).
    #[allow(dead_code)]
    Dmabuf { width: u32, height: u32 },
}

impl Frame {
    pub fn dimensions(&self) -> (u32, u32) {
        match self.buf {
            FrameBuf::Cpu { width, height, .. } | FrameBuf::Dmabuf { width, height } => {
                (width, height)
            }
        }
    }
}

pub enum CaptureEvent {
    Frame(Frame),
    /// Capture stopped on its own; the display is unusable until reopened.
    Ended(String),
}

pub type FrameSink = Box<dyn FnMut(CaptureEvent) + Send>;

/// The pointer cursor over a display, for the cursor side channel.
#[derive(Debug, Clone, PartialEq)]
pub enum CursorEvent {
    /// Hotspot position, normalized to the display (PROTOCOL §7).
    Moved { x: f64, y: f64 },
    /// The cursor left the display (or has no image).
    Hidden,
    /// A new sprite: PNG bytes, its size normalized to the display, the
    /// hotspot normalized within the sprite.
    Image {
        png: Vec<u8>,
        nw: f64,
        nh: f64,
        ax: f64,
        ay: f64,
    },
}

pub type CursorSink = Box<dyn FnMut(CursorEvent) + Send>;

pub trait InputSink: Send {
    fn touch(&mut self, t: &Touch);
    fn scroll(&mut self, s: &Scroll);
    /// Pointer mapping lands with SENDER-PLAN §5 step 3; dropped until then.
    fn pencil(&mut self, _p: &Pencil) {}
}

#[cfg(test)]
mod tests {
    use super::Position;

    #[test]
    fn positions_parse() {
        assert_eq!(
            "auto-right".parse::<Position>().unwrap(),
            Position::AutoRight
        );
        assert_eq!("auto-up".parse::<Position>().unwrap(), Position::AutoUp);
        assert_eq!(
            "-1600,0".parse::<Position>().unwrap(),
            Position::At(-1600, 0)
        );
        assert_eq!("3024x0".parse::<Position>().unwrap(), Position::At(3024, 0));
        assert!("center".parse::<Position>().is_err());
    }
}
