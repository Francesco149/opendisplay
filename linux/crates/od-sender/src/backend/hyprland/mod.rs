//! Hyprland >= 0.55: headless outputs over the IPC socket, captured and
//! driven through the shared `wayland/*` protocols.

pub mod display;
pub mod instance;
pub mod ipc;

use std::path::PathBuf;

use anyhow::{Result, bail};
use tracing::warn;

use self::display::HyprlandOutput;
use self::ipc::HyprlandIpc;
use super::lock::{lock_output, try_lock};
use super::{Backend, BackendId, Caps, DisplaySpec, VirtualDisplay};
use crate::wayland::display::CapturedDisplay;

/// Oldest release with the Lua `eval` path the sender configures outputs with.
pub const MIN_VERSION: (u32, u32) = (0, 55);
/// Newest minor release the sender was run against.
pub const TESTED_UP_TO: (u32, u32) = (0, 56);

pub struct HyprlandBackend {
    socket: PathBuf,
    version: String,
}

impl HyprlandBackend {
    /// Connect to the instance behind `socket` and refuse versions without
    /// the Lua IPC.
    pub fn connect(socket: PathBuf) -> Result<Self> {
        let ipc = HyprlandIpc::new(socket.clone());
        let v = ipc.version_info()?;
        match v.triple() {
            Some((maj, min, _)) if (maj, min) < MIN_VERSION => bail!(
                "Hyprland {} is too old: od-sender needs {}.{} or newer (Lua IPC)",
                v.version,
                MIN_VERSION.0,
                MIN_VERSION.1
            ),
            Some((maj, min, _)) if (maj, min) > TESTED_UP_TO => warn!(
                "Hyprland {} is newer than the tested {}.{}.x; report problems with `od-sender doctor` output",
                v.version, TESTED_UP_TO.0, TESTED_UP_TO.1
            ),
            Some(_) => {}
            None => warn!("cannot parse Hyprland version {:?}", v.version),
        }
        Ok(Self {
            socket,
            version: v.version,
        })
    }

    pub fn ipc(&self) -> HyprlandIpc {
        HyprlandIpc::new(self.socket.clone())
    }
}

impl Backend for HyprlandBackend {
    fn id(&self) -> BackendId {
        BackendId::Hyprland
    }

    fn describe(&self) -> String {
        format!("Hyprland {} (IPC {})", self.version, self.socket.display())
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
        let lock = lock_output(&spec.name)?;
        let output = HyprlandOutput::open(self.ipc(), spec)?;
        Ok(Box::new(CapturedDisplay::new(output, lock)))
    }

    fn clean(&mut self) -> Result<Vec<String>> {
        let ipc = self.ipc();
        let mut removed = Vec::new();
        for m in ipc.monitors()? {
            if !m.name.starts_with("od-") {
                continue;
            }
            // Held while removing, so no sender can start on it meanwhile.
            let Some(_lock) = try_lock(&m.name)? else {
                warn!("{} is in use by a running od-sender; leaving it", m.name);
                continue;
            };
            ipc.remove_output(&m.name)?;
            removed.push(m.name);
        }
        Ok(removed)
    }
}
