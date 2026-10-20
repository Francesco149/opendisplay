//! GNOME (Mutter): a virtual monitor from Mutter's ScreenCast API, input
//! through its RemoteDesktop API, frames from the stream's PipeWire node.
//! No portal and no dialog for an unsandboxed process (see `mutter.rs`).
//! Mutter removes the monitor when the session stops or this process's bus
//! connection closes, so nothing is ever left behind and `--keep-output`
//! cannot apply.

pub mod input;
pub mod mutter;

use anyhow::{Result, bail};
use tracing::{info, warn};

use self::input::{Mapper, RemoteInput};
use self::mutter::{CursorMode, Request, VirtualMonitor};
use super::lock::{OutputLock, lock_output};
use super::{
    Backend, BackendId, Caps, CaptureOptions, DisplayInfo, DisplaySpec, FrameSink, InputSink,
    VirtualDisplay,
};
use crate::pipewire::{PwCapture, PwSource};

pub struct GnomeBackend {
    versions: (i32, i32),
}

impl GnomeBackend {
    /// Connect to the session bus and check that Mutter's APIs are there.
    pub fn connect() -> Result<Self> {
        let conn = zbus::blocking::Connection::session()?;
        let versions = mutter::probe(&conn)?;
        Ok(Self { versions })
    }
}

impl Backend for GnomeBackend {
    fn id(&self) -> BackendId {
        BackendId::Gnome
    }

    fn describe(&self) -> String {
        format!(
            "GNOME Mutter (ScreenCast API v{}, RemoteDesktop API v{})",
            self.versions.0, self.versions.1
        )
    }

    fn caps(&self) -> Caps {
        Caps {
            // Mutter offers cursor metadata on the PipeWire stream; not read yet.
            cursor_metadata: false,
            dmabuf: false,
            // A stream created with `modes` has a fixed size: a new size is a
            // new monitor.
            resize_in_place: false,
            creates_outputs: true,
        }
    }

    fn open(&mut self, spec: &DisplaySpec) -> Result<Box<dyn VirtualDisplay>> {
        let lock = lock_output(&spec.name)?;
        let vm = VirtualMonitor::create(&request(spec))?;
        Ok(Box::new(GnomeDisplay::new(spec, vm, lock)))
    }

    fn clean(&mut self) -> Result<Vec<String>> {
        // Mutter removes a virtual monitor with its session; nothing outlives us.
        Ok(Vec::new())
    }
}

fn request(spec: &DisplaySpec) -> Request {
    Request {
        width: spec.width,
        height: spec.height,
        hz: spec.hz,
        cursor: CursorMode::Embedded,
    }
}

fn info_of(name: &str, vm: &VirtualMonitor) -> DisplayInfo {
    let p = &vm.placement;
    DisplayInfo {
        name: name.to_string(),
        width: p.width,
        height: p.height,
        refresh_hz: p.refresh,
        scale: p.scale,
        x: p.x,
        y: p.y,
    }
}

pub struct GnomeDisplay {
    /// Field order is drop order: capture before the monitor.
    capture: Option<PwCapture>,
    vm: Option<VirtualMonitor>,
    info: DisplayInfo,
    _lock: OutputLock,
}

impl GnomeDisplay {
    fn new(spec: &DisplaySpec, vm: VirtualMonitor, lock: OutputLock) -> Self {
        let info = info_of(&spec.name, &vm);
        info!(
            "virtual display {} ({}, PipeWire node {})",
            info, vm.placement.connector, vm.node
        );
        Self {
            capture: None,
            info,
            vm: Some(vm),
            _lock: lock,
        }
    }
}

impl VirtualDisplay for GnomeDisplay {
    fn info(&self) -> &DisplayInfo {
        &self.info
    }

    /// The cursor is part of the stream (`cursor-mode` embedded), fixed when
    /// the monitor was created; `opts.cursor` does not change it.
    fn start_capture(&mut self, _opts: CaptureOptions, sink: FrameSink) -> Result<()> {
        self.stop_capture();
        let Some(vm) = &self.vm else {
            bail!("no virtual monitor");
        };
        // Mutter's stream lives on the session's PipeWire daemon. Asking for
        // the monitor's size is what sizes it on Mutter < 50 (no `modes`).
        self.capture = Some(PwCapture::spawn(
            PwSource {
                node: vm.node,
                serial: None,
                remote_fd: None,
            },
            (self.info.width, self.info.height),
            sink,
        )?);
        Ok(())
    }

    fn stop_capture(&mut self) {
        if let Some(mut c) = self.capture.take() {
            c.stop();
        }
    }

    fn reconfigure(&mut self, spec: &DisplaySpec) -> Result<()> {
        self.stop_capture();
        if let Some(mut old) = self.vm.take() {
            if let Err(e) = old.stop() {
                warn!("stopping the old virtual monitor: {e:#}");
            }
        }
        let vm = VirtualMonitor::create(&request(spec))?;
        self.info = info_of(&spec.name, &vm);
        info!("virtual display {} ({})", self.info, vm.placement.connector);
        self.vm = Some(vm);
        Ok(())
    }

    fn input(&mut self) -> Result<Box<dyn InputSink>> {
        let Some(vm) = &self.vm else {
            bail!("no virtual monitor");
        };
        let p = &vm.placement;
        Ok(Box::new(RemoteInput::start(
            vm.input(),
            Mapper::new(p.width, p.height, p.scale),
        )?))
    }

    fn close(mut self: Box<Self>, keep_output: bool) -> Result<()> {
        if keep_output {
            warn!("--keep-output: Mutter removes a virtual monitor with its session");
        }
        self.stop_capture();
        if let Some(mut vm) = self.vm.take() {
            vm.stop()?;
            info!("removed virtual display {}", self.info.name);
        }
        Ok(())
    }
}

impl Drop for GnomeDisplay {
    fn drop(&mut self) {
        self.stop_capture();
        // VirtualMonitor's own Drop stops the session.
    }
}
