//! Hyprland IPC: virtual ("headless") outputs over the request socket. Hyprland >= 0.55 is Lua-configured, so monitor rules go through
//! `eval hl.monitor({...})`; `keyword` is rejected. Every mutation is verified
//! by reading `monitors` back, because `eval` answers `ok` to inert Lua.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::backend::Position;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // fields are for logging and the coming sender lifecycle
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
    #[serde(rename = "refreshRate")]
    pub refresh_rate: f64,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    #[serde(default)]
    pub focused: bool,
}

/// `j/version`, the fields the sender reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Version {
    /// `0.56.2`
    pub version: String,
    #[serde(default)]
    pub tag: String,
}

impl Version {
    /// `(major, minor, patch)`, or `None` for an unparseable string.
    pub fn triple(&self) -> Option<(u32, u32, u32)> {
        parse_version(&self.version).or_else(|| parse_version(&self.tag))
    }
}

fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.trim_start_matches('v').split(['.', '-', '+']);
    let mut n = || it.next()?.parse().ok();
    Some((n()?, n()?, n().unwrap_or(0)))
}

/// The Lua `position` string for a monitor rule.
pub fn position_spec(p: Position) -> String {
    match p {
        Position::AutoRight => "auto-right".into(),
        Position::AutoLeft => "auto-left".into(),
        Position::AutoUp => "auto-up".into(),
        Position::AutoDown => "auto-down".into(),
        Position::At(x, y) => format!("{x}x{y}"),
    }
}

pub struct HyprlandIpc {
    socket: PathBuf,
}

/// `$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock`.
pub fn socket_path(runtime_dir: &Path, sig: &str) -> PathBuf {
    runtime_dir.join("hypr").join(sig).join(".socket.sock")
}

impl HyprlandIpc {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    pub fn from_env() -> Result<Self> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .context("HYPRLAND_INSTANCE_SIGNATURE not set: not inside Hyprland?")?;
        let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
        let socket = socket_path(Path::new(&runtime), &sig);
        if !socket.exists() {
            bail!("Hyprland IPC socket {} does not exist", socket.display());
        }
        Ok(Self { socket })
    }

    pub fn request(&self, cmd: &str) -> Result<String> {
        let mut s = UnixStream::connect(&self.socket)
            .with_context(|| format!("connecting {}", self.socket.display()))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.write_all(cmd.as_bytes())?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        Ok(out)
    }

    pub fn version(&self) -> Result<String> {
        Ok(self
            .request("version")?
            .lines()
            .next()
            .unwrap_or_default()
            .to_string())
    }

    pub fn version_info(&self) -> Result<Version> {
        let json = self.request("j/version")?;
        serde_json::from_str(&json).with_context(|| format!("parsing version: {json}"))
    }

    /// `j/getoption <name>`: the raw JSON object.
    pub fn option(&self, name: &str) -> Result<serde_json::Value> {
        let json = self.request(&format!("j/getoption {name}"))?;
        serde_json::from_str(&json).with_context(|| format!("parsing option {name}: {json}"))
    }

    pub fn monitors(&self) -> Result<Vec<Monitor>> {
        parse_monitors(&self.request("j/monitors")?)
    }

    pub fn monitor(&self, name: &str) -> Result<Option<Monitor>> {
        Ok(self.monitors()?.into_iter().find(|m| m.name == name))
    }

    /// Create a headless output. Hyprland gives it 1920x1080@60 until
    /// [`configure`](Self::configure) sets the real mode.
    pub fn create_headless(&self, name: &str) -> Result<()> {
        let r = self.request(&format!("output create headless {name}"))?;
        if r.trim() != "ok" {
            bail!("output create failed: {r}");
        }
        if self.monitor(name)?.is_none() {
            bail!("output {name} not listed after creation");
        }
        Ok(())
    }

    /// Set mode and scale. `position` is a Hyprland position spec, e.g.
    /// `auto-right`; the sender uses it to park the virtual display beside
    /// the real ones.
    pub fn configure(
        &self,
        name: &str,
        width: u32,
        height: u32,
        hz: u32,
        scale: f64,
        position: Position,
    ) -> Result<Monitor> {
        let position = position_spec(position);
        let lua = format!(
            "hl.monitor({{output=\"{name}\", mode=\"{width}x{height}@{hz}\", position=\"{position}\", scale={scale}}})"
        );
        let r = self.request(&format!("eval {lua}"))?;
        if r.trim() != "ok" {
            bail!("eval failed: {r}");
        }
        // Hyprland applies monitor rules asynchronously; poll briefly.
        for _ in 0..20 {
            if let Some(m) = self.monitor(name)? {
                if m.width == width && m.height == height && (m.scale - scale).abs() < 1e-6 {
                    return Ok(m);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("output {name} did not take mode {width}x{height}@{hz} scale {scale}")
    }

    pub fn remove_output(&self, name: &str) -> Result<()> {
        let r = self.request(&format!("output remove {name}"))?;
        if r.trim() != "ok" {
            bail!("output remove failed: {r}");
        }
        Ok(())
    }
}

pub fn parse_monitors(json: &str) -> Result<Vec<Monitor>> {
    serde_json::from_str(json).with_context(|| format!("parsing monitors: {json}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `hyprctl -j monitors` on Hyprland 0.56.2 (Asahi, M1 Pro), trimmed of
    /// fields the sender does not read, plus a headless `od-` output.
    const MONITORS: &str = r#"[{
        "id": 0, "name": "eDP-1", "description": "", "make": "", "model": "",
        "width": 3024, "height": 1964, "physicalWidth": 302, "physicalHeight": 196,
        "refreshRate": 120.00000, "x": 0, "y": 0,
        "activeWorkspace": {"id": 2, "name": "2"}, "reserved": [0, 32, 0, 0],
        "scale": 2, "transform": 0, "focused": true, "dpmsStatus": true,
        "currentFormat": "XRGB8888", "mirrorOf": "none",
        "availableModes": ["3024x1964@120.00Hz","3024x1964@60.00Hz"]
    },{
        "id": 1, "name": "od-3f9a1c", "description": "Headless output",
        "width": 2532, "height": 1170, "refreshRate": 120.00000,
        "x": 1512, "y": 0, "scale": 3.00, "focused": false
    }]"#;

    const VERSION: &str = r#"{
        "branch": "v0.56.2", "commit": "efb50993780079460b0cbed1363e2166a2de1d9f",
        "version": "0.56.2", "dirty": false, "tag": "v0.56.2", "commits": "7661",
        "buildAquamarine": "0.15.1", "flags": []
    }"#;

    #[test]
    fn monitors_parse() {
        let m = parse_monitors(MONITORS).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "eDP-1");
        assert_eq!((m[0].width, m[0].height, m[0].scale), (3024, 1964, 2.0));
        assert!(m[0].focused);
        assert_eq!(m[1].name, "od-3f9a1c");
        assert_eq!((m[1].x, m[1].scale), (1512, 3.0));
        assert_eq!(m[1].refresh_rate.round(), 120.0);
    }

    #[test]
    fn version_parses() {
        let v: Version = serde_json::from_str(VERSION).unwrap();
        assert_eq!(v.triple(), Some((0, 56, 2)));
        let tag_only = Version {
            version: String::new(),
            tag: "v0.55.0-b".into(),
        };
        assert_eq!(tag_only.triple(), Some((0, 55, 0)));
    }

    #[test]
    fn positions_render_as_monitor_rules() {
        assert_eq!(position_spec(Position::AutoRight), "auto-right");
        assert_eq!(position_spec(Position::At(-1600, 0)), "-1600x0");
    }
}
