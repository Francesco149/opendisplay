//! KDE Plasma (KWin): a virtual output requested through
//! `zkde_screencast_unstable_v1`, its frames from the PipeWire node KWin
//! creates for it, input through `org_kde_kwin_fake_input`. No dialog and
//! no portal. KWin removes the output when the stream closes or the process
//! dies, so nothing is ever left behind and `--keep-output` cannot work.

pub mod input;
pub mod protocols;
pub mod screencast;

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tracing::{debug, info, warn};

use self::input::FakeInput;
use self::screencast::{Pointer, Request, VirtualOutputStream};
use super::lock::{OutputLock, lock_output};
use super::{
    Backend, BackendId, Caps, CaptureOptions, DisplayInfo, DisplaySpec, FrameSink, InputSink,
    VirtualDisplay,
};
use crate::pipewire::{PwCapture, PwSource};
use crate::wayland::registry::{self, OutputSnapshot};

/// KWin names a virtual output `Virtual-<name>`.
pub fn output_name(lock_name: &str) -> String {
    format!("Virtual-{lock_name}")
}

pub struct KdeBackend;

impl Backend for KdeBackend {
    /// Frames come over PipeWire, not a Wayland capture protocol.
    fn wayland_capture(&self) -> bool {
        false
    }

    fn set_capture(&mut self, _: super::capture::CapturePath) {}

    fn id(&self) -> BackendId {
        BackendId::Kde
    }

    fn describe(&self) -> String {
        "KWin (zkde_screencast virtual output, PipeWire, fake input)".into()
    }

    fn caps(&self) -> Caps {
        Caps {
            cursor_metadata: false,
            dmabuf: false,
            // zkde has no resize: a new size is a new output.
            resize_in_place: false,
            creates_outputs: true,
        }
    }

    fn open(&mut self, spec: &DisplaySpec) -> Result<Box<dyn VirtualDisplay>> {
        let lock = lock_output(&spec.name)?;
        let mut d = KdeDisplay {
            capture: None,
            stream: None,
            pointer: Pointer::Embedded,
            spec: spec.clone(),
            info: DisplayInfo {
                name: output_name(&spec.name),
                width: spec.width,
                height: spec.height,
                refresh_hz: 60.0,
                scale: spec.scale,
                x: 0,
                y: 0,
            },
            _lock: lock,
        };
        d.create()?;
        Ok(Box::new(d))
    }

    /// KWin drops a virtual output with its client; there are no leftovers.
    fn clean(&mut self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

pub struct KdeDisplay {
    // Field order is drop order: capture before the output it reads.
    capture: Option<PwCapture>,
    stream: Option<VirtualOutputStream>,
    pointer: Pointer,
    spec: DisplaySpec,
    info: DisplayInfo,
    _lock: OutputLock,
}

/// The size to request so the mode comes out `want` pixels, given that a
/// request of `asked` produced `got`. The protocol says logical size, and
/// KWin's virtual backend multiplies by the scale; its DRM backend (real
/// sessions, 6.8) uses the request as the mode as is. One correction from
/// the read-back covers both.
pub fn corrected(want: (u32, u32), asked: (i32, i32), got: (i32, i32)) -> (i32, i32) {
    let fix = |w: u32, a: i32, g: i32| {
        if g <= 0 {
            a
        } else {
            (a as f64 * w as f64 / g as f64).round() as i32
        }
    };
    (fix(want.0, asked.0, got.0), fix(want.1, asked.1, got.1))
}

impl KdeDisplay {
    /// One virtual output of `width`x`height` (as zkde means it); waits for
    /// its wl_output to get a mode and place.
    fn make(&mut self, width: i32, height: i32) -> Result<OutputSnapshot> {
        self.stop_capture();
        self.stream = None;
        let name = output_name(&self.spec.name);
        self.stream = Some(VirtualOutputStream::open(Request {
            name: self.spec.name.clone(),
            description: "OpenDisplay".into(),
            width,
            height,
            scale: self.spec.scale,
            pointer: self.pointer,
        })?);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snap = registry::snapshot()?;
            if let Some(o) = snap.output(&name) {
                if o.width > 0 {
                    return Ok(o.clone());
                }
            }
            if Instant::now() > deadline {
                bail!("KWin made no output {name}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// (Re)create the output for `self.spec` and read its layout back.
    fn create(&mut self) -> Result<()> {
        let want = (self.spec.width, self.spec.height);
        let asked = (
            (want.0 as f64 / self.spec.scale).round() as i32,
            (want.1 as f64 / self.spec.scale).round() as i32,
        );
        let mut o = self.make(asked.0, asked.1)?;
        if (o.width as u32, o.height as u32) != want {
            let again = corrected(want, asked, (o.width, o.height));
            debug!(
                "{}: asked {}x{} got {}x{}; asking {}x{}",
                o.name, asked.0, asked.1, o.width, o.height, again.0, again.1
            );
            o = self.make(again.0, again.1)?;
        }
        if (o.width as u32, o.height as u32) != want {
            warn!(
                "{} is {}x{}, wanted {}x{}",
                o.name, o.width, o.height, want.0, want.1
            );
        }
        let scale = o.fractional_scale();
        if (scale - self.spec.scale).abs() > 0.01 {
            // KWin picks the scale of a new output from its DPI or from what
            // the user saved for this output name in System Settings.
            info!(
                "KWin set {} to scale {scale} (asked {}); change it once in System Settings > Display, KWin remembers it per output",
                o.name, self.spec.scale
            );
        }
        self.info = DisplayInfo {
            name: o.name.clone(),
            width: o.width as u32,
            height: o.height as u32,
            refresh_hz: o.refresh_mhz as f64 / 1000.0,
            scale,
            x: o.x,
            y: o.y,
        };
        info!("virtual display {}", self.info);
        Ok(())
    }
}

impl VirtualDisplay for KdeDisplay {
    fn info(&self) -> &DisplayInfo {
        &self.info
    }

    fn start_capture(&mut self, opts: CaptureOptions, sink: FrameSink) -> Result<()> {
        self.stop_capture();
        // KWin fixes the pointer mode when the stream is made.
        let want = if opts.cursor {
            Pointer::Embedded
        } else {
            Pointer::Hidden
        };
        if want != self.pointer {
            info!("recreating {} for pointer mode {want:?}", self.info.name);
            self.pointer = want;
            self.create()?;
        }
        let Some(stream) = &self.stream else {
            bail!("{} is gone", self.info.name);
        };
        let size = (self.info.width, self.info.height);
        let src = PwSource {
            node: stream.node.id,
            serial: stream.node.serial,
            remote_fd: None,
        };
        self.capture = Some(PwCapture::spawn(src, size, sink)?);
        Ok(())
    }

    fn stop_capture(&mut self) {
        if let Some(mut c) = self.capture.take() {
            c.stop();
        }
    }

    fn reconfigure(&mut self, spec: &DisplaySpec) -> Result<()> {
        self.spec = spec.clone();
        self.create()
    }

    fn input(&mut self) -> Result<Box<dyn InputSink>> {
        Ok(Box::new(FakeInput::new(&self.info)?))
    }

    fn close(mut self: Box<Self>, keep_output: bool) -> Result<()> {
        if keep_output {
            warn!(
                "KWin removes {} with its stream; --keep-output has no effect on KDE",
                self.info.name
            );
        }
        self.stop_capture();
        self.stream = None;
        info!("removed virtual display {}", self.info.name);
        Ok(())
    }
}

impl Drop for KdeDisplay {
    fn drop(&mut self) {
        self.stop_capture();
        self.stream = None;
    }
}

#[cfg(test)]
mod tests {
    use super::corrected;

    #[test]
    fn size_correction_covers_both_kwin_backends() {
        // Virtual backend: logical request, mode = request * scale.
        assert_eq!(corrected((1024, 768), (512, 384), (1024, 768)), (512, 384));
        // DRM backend: the request is the mode.
        assert_eq!(corrected((1024, 768), (512, 384), (512, 384)), (1024, 768));
        // Scale 1.5 on the virtual backend.
        assert_eq!(
            corrected((1920, 1200), (1280, 800), (1920, 1200)),
            (1280, 800)
        );
    }
}
