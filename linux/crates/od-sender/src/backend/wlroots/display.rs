//! The Sway side of a virtual display: a headless output made with
//! `create_output`, sized and placed through `zwlr_output_manager_v1`, and
//! removed with `output <name> unplug`. Capture ordering is enforced by
//! [`crate::wayland::display::CapturedDisplay`], which wraps this.

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use super::ipc::{Rect, SwayIpc};
use super::outman::{self, Target};
use super::record::{self, Record};
use crate::backend::{DisplayInfo, DisplaySpec, Position};
use crate::wayland::display::OutputControl;

pub struct WlrootsOutput {
    ipc: SwayIpc,
    /// `od-<id>`: the lock and record key.
    lock_name: String,
    info: DisplayInfo,
}

/// Top-left of a `w`x`h` (logical) output placed next to `others`.
pub fn place(position: Position, w: i32, h: i32, others: &[Rect]) -> (i32, i32) {
    if others.is_empty() {
        return match position {
            Position::At(x, y) => (x, y),
            _ => (0, 0),
        };
    }
    let right = |r: &Rect| r.x + r.width;
    let bottom = |r: &Rect| r.y + r.height;
    match position {
        Position::At(x, y) => (x, y),
        Position::AutoRight => {
            let edge = others.iter().max_by_key(|r| right(r)).unwrap();
            (right(edge), edge.y)
        }
        Position::AutoLeft => {
            let edge = others.iter().min_by_key(|r| r.x).unwrap();
            (edge.x - w, edge.y)
        }
        Position::AutoDown => {
            let edge = others.iter().max_by_key(|r| bottom(r)).unwrap();
            (edge.x, bottom(edge))
        }
        Position::AutoUp => {
            let edge = others.iter().min_by_key(|r| r.y).unwrap();
            (edge.x, edge.y - h)
        }
    }
}

impl WlrootsOutput {
    /// Reuse the output a dead sender left for this display (the caller holds
    /// its lock), or create one; then size it.
    pub fn open(ipc: SwayIpc, spec: &DisplaySpec) -> Result<Self> {
        let sock = ipc.socket().to_path_buf();
        let leftover = record::load(&spec.name)
            .filter(|r| r.belongs_to(&sock))
            .map(|r| r.output)
            .filter(|name| matches!(ipc.output(name), Ok(Some(_))));
        let name = match leftover {
            Some(name) => {
                info!("reusing {name}, left behind for {}", spec.name);
                name
            }
            None => ipc.create_output().context("creating a headless output")?,
        };
        record::save(
            &spec.name,
            &Record {
                output: name.clone(),
                swaysock: sock,
            },
        )?;
        let mut out = Self {
            ipc,
            lock_name: spec.name.clone(),
            info: DisplayInfo {
                name,
                width: 0,
                height: 0,
                refresh_hz: 0.0,
                scale: 1.0,
                x: 0,
                y: 0,
            },
        };
        if let Err(e) = out.reconfigure(spec) {
            if let Err(r) = out.remove() {
                warn!("removing {} after a failed setup: {r:#}", out.info.name);
            }
            return Err(e);
        }
        Ok(out)
    }
}

impl OutputControl for WlrootsOutput {
    fn info(&self) -> &DisplayInfo {
        &self.info
    }

    fn reconfigure(&mut self, spec: &DisplaySpec) -> Result<()> {
        let name = self.info.name.clone();
        let outputs = self.ipc.outputs()?;
        let others: Vec<Rect> = outputs
            .iter()
            .filter(|o| o.active && o.name != name)
            .map(|o| o.rect)
            .collect();
        let (lw, lh) = (
            (spec.width as f64 / spec.scale).round() as i32,
            (spec.height as f64 / spec.scale).round() as i32,
        );
        let (x, y) = place(spec.position, lw, lh, &others);
        outman::apply(&Target {
            name: name.clone(),
            width: spec.width as i32,
            height: spec.height as i32,
            refresh_mhz: (spec.hz * 1000) as i32,
            scale: spec.scale,
            x,
            y,
        })?;
        let o = self
            .ipc
            .output(&name)?
            .with_context(|| format!("{name} vanished while being configured"))?;
        let mode = o.current_mode.unwrap_or_default();
        if (mode.width, mode.height) != (spec.width, spec.height) {
            bail!(
                "{name} is {}x{} after asking for {}x{}",
                mode.width,
                mode.height,
                spec.width,
                spec.height
            );
        }
        self.info = DisplayInfo {
            name,
            width: mode.width,
            height: mode.height,
            refresh_hz: mode.refresh as f64 / 1000.0,
            scale: o.scale.unwrap_or(spec.scale),
            x: o.rect.x,
            y: o.rect.y,
        };
        info!("virtual display {}", self.info);
        Ok(())
    }

    fn remove(&mut self) -> Result<()> {
        self.ipc.unplug(&self.info.name)?;
        record::remove(&self.lock_name);
        info!("removed virtual display {}", self.info.name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn placement_next_to_the_layout() {
        let laptop = r(0, 0, 1512, 982);
        let left = r(-1920, -100, 1920, 1080);
        let both = [laptop, left];
        assert_eq!(place(Position::AutoRight, 800, 600, &both), (1512, 0));
        assert_eq!(place(Position::AutoLeft, 800, 600, &both), (-2720, -100));
        assert_eq!(place(Position::AutoDown, 800, 600, &both), (0, 982));
        assert_eq!(place(Position::AutoUp, 800, 600, &both), (-1920, -700));
        assert_eq!(place(Position::At(5, 6), 800, 600, &both), (5, 6));
        assert_eq!(place(Position::AutoRight, 800, 600, &[]), (0, 0));
    }
}
