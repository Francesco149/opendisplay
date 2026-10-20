//! The Hyprland side of a virtual display: a headless output created, sized
//! and removed over IPC. Capture ordering is enforced by
//! [`crate::wayland::display::CapturedDisplay`], which wraps this.

use anyhow::{Context, Result};
use tracing::info;

use super::ipc::{HyprlandIpc, Monitor};
use crate::backend::{DisplayInfo, DisplaySpec};
use crate::wayland::cursor::CursorSpace;
use crate::wayland::display::OutputControl;

pub struct HyprlandOutput {
    ipc: HyprlandIpc,
    info: DisplayInfo,
}

fn info_of(m: &Monitor) -> DisplayInfo {
    DisplayInfo {
        name: m.name.clone(),
        width: m.width,
        height: m.height,
        refresh_hz: m.refresh_rate,
        scale: m.scale,
        x: m.x,
        y: m.y,
    }
}

impl HyprlandOutput {
    /// Create the output unless a leftover of the same name exists (the
    /// caller holds its lock, so no sender is using it), then size it.
    pub fn open(ipc: HyprlandIpc, spec: &DisplaySpec) -> Result<Self> {
        let mut out = Self {
            info: DisplayInfo {
                name: spec.name.clone(),
                width: 0,
                height: 0,
                refresh_hz: 0.0,
                scale: 1.0,
                x: 0,
                y: 0,
            },
            ipc,
        };
        out.reconfigure(spec)?;
        Ok(out)
    }
}

impl OutputControl for HyprlandOutput {
    fn info(&self) -> &DisplayInfo {
        &self.info
    }

    fn reconfigure(&mut self, spec: &DisplaySpec) -> Result<()> {
        let name = &spec.name;
        if self.ipc.monitor(name)?.is_none() {
            self.ipc
                .create_headless(name)
                .with_context(|| format!("creating headless output {name}"))?;
        }
        let m = self.ipc.configure(
            name,
            spec.width,
            spec.height,
            spec.hz,
            spec.scale,
            spec.position,
        )?;
        self.info = info_of(&m);
        info!("virtual display {}", self.info);
        Ok(())
    }

    /// Hyprland 0.56.2 reports cursor positions and hotspots in logical
    /// units and renders the sprite at the highest monitor scale
    /// (`CImageCopyCaptureCursorSession::sendCursorEvents`,
    /// `CCursorManager::updateTheme`).
    fn cursor_space(&self) -> CursorSpace {
        let scale = self.info.scale.max(0.1);
        let highest = self
            .ipc
            .monitors()
            .map(|ms| ms.iter().map(|m| m.scale).fold(1.0f64, f64::max))
            .unwrap_or(scale)
            .max(1.0);
        CursorSpace {
            width: self.info.width as f64 / scale,
            height: self.info.height as f64 / scale,
            sprite_scale: highest,
            theme_size: crate::wayland::cursor::theme_size(),
        }
    }

    fn remove(&mut self) -> Result<()> {
        self.ipc.remove_output(&self.info.name)?;
        info!("removed virtual display {}", self.info.name);
        Ok(())
    }
}
