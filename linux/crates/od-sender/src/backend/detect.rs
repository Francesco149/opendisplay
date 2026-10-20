//! Startup probing (SENDER-PLAN §3.3): pick a backend, or refuse with a
//! message that names the gap. [`decide`] is pure over an [`Environment`] and
//! a registry probe so the walk is unit-tested; `doctor` prints the same walk.

use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use super::adopt::AdoptBackend;
use super::hyprland::{HyprlandBackend, ipc};
use super::wlroots::WlrootsBackend;
use super::{Backend, BackendId};
use crate::wayland::registry::Snapshot;

pub const ISSUE: &str = "https://github.com/peetzweg/opendisplay/issues/299";

/// The process environment detection reads.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    pub runtime_dir: Option<PathBuf>,
    pub hyprland_signature: Option<String>,
    pub swaysock: Option<String>,
    pub wayland_display: Option<String>,
    pub session_type: Option<String>,
    pub current_desktop: Option<String>,
    pub kde_full_session: bool,
}

impl Environment {
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            runtime_dir: var("XDG_RUNTIME_DIR").map(PathBuf::from),
            hyprland_signature: var("HYPRLAND_INSTANCE_SIGNATURE"),
            swaysock: var("SWAYSOCK"),
            wayland_display: var("WAYLAND_DISPLAY"),
            session_type: var("XDG_SESSION_TYPE"),
            current_desktop: var("XDG_CURRENT_DESKTOP"),
            kde_full_session: var("KDE_FULL_SESSION").is_some(),
        }
    }
}

/// What the user asked for on the command line.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub backend: Option<BackendId>,
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Hyprland { socket: PathBuf },
    Wlroots { swaysock: PathBuf },
    Adopt { output: String },
    Unsupported(String),
}

/// The decision plus every probe that led to it, for `doctor`.
#[derive(Debug, Clone)]
pub struct Walk {
    pub steps: Vec<String>,
    pub decision: Decision,
}

fn has_capture(s: &Snapshot) -> bool {
    (s.has("ext_image_copy_capture_manager_v1")
        && s.has("ext_output_image_capture_source_manager_v1"))
        || s.has("zwlr_screencopy_manager_v1")
}

fn has_input(s: &Snapshot) -> bool {
    s.has("zwlr_virtual_pointer_manager_v1")
}

/// What the wlroots backend needs beyond IPC: `capture.rs` speaks only
/// ext-image-copy-capture (Sway >= 1.10), and sizing goes through the output
/// manager.
fn wlroots_missing(s: &Snapshot) -> Vec<&'static str> {
    [
        "ext_image_copy_capture_manager_v1",
        "ext_output_image_capture_source_manager_v1",
        "zwlr_virtual_pointer_manager_v1",
        "zwlr_output_manager_v1",
    ]
    .into_iter()
    .filter(|g| !s.has(g))
    .collect()
}

/// Walk §3.3 in order, stopping at the first hit. `probe` is the Wayland
/// registry roundtrip; it runs at most once and only when needed.
pub fn decide(
    env: &Environment,
    req: &Request,
    probe: &mut dyn FnMut() -> Result<Snapshot>,
) -> Walk {
    let mut steps = Vec::new();
    let mut snapshot: Option<Result<Snapshot, String>> = None;
    let mut registry = |steps: &mut Vec<String>| -> Result<Snapshot, String> {
        snapshot
            .get_or_insert_with(|| {
                let r = probe().map_err(|e| format!("{e:#}"));
                match &r {
                    Ok(s) => steps.push(format!(
                        "wayland registry ({}): {} globals, capture {}, virtual pointer {}",
                        env.wayland_display.as_deref().unwrap_or("?"),
                        s.globals.len(),
                        yes(has_capture(s)),
                        yes(has_input(s)),
                    )),
                    Err(e) => steps.push(format!("wayland registry: {e}")),
                }
                r
            })
            .clone()
    };
    let done = |steps: Vec<String>, decision| Walk { steps, decision };

    let hyprland_socket = match (&env.hyprland_signature, &env.runtime_dir) {
        (Some(sig), Some(rt)) => {
            let socket = ipc::socket_path(rt, sig);
            let exists = socket.exists();
            steps.push(format!(
                "HYPRLAND_INSTANCE_SIGNATURE={sig}: socket {} {}",
                socket.display(),
                if exists { "exists" } else { "missing" }
            ));
            exists.then_some(socket)
        }
        (Some(_), None) => {
            steps.push("HYPRLAND_INSTANCE_SIGNATURE set but XDG_RUNTIME_DIR is not".into());
            None
        }
        (None, _) => {
            steps.push("HYPRLAND_INSTANCE_SIGNATURE not set".into());
            None
        }
    };

    // 1. Explicit choice.
    let adopt =
        |output: &String,
         steps: &mut Vec<String>,
         registry: &mut dyn FnMut(&mut Vec<String>) -> Result<Snapshot, String>| {
            match registry(steps) {
                Ok(s) if has_capture(&s) && has_input(&s) => Decision::Adopt {
                    output: output.clone(),
                },
                Ok(_) => Decision::Unsupported(format!(
                    "--output {output}: this compositor lacks ext-image-copy-capture or zwlr_virtual_pointer, which adopt mode needs"
                )),
                Err(e) => {
                    Decision::Unsupported(format!("--output {output}: no Wayland connection ({e})"))
                }
            }
        };
    match (req.backend, &req.output) {
        (Some(BackendId::Adopt) | None, Some(output)) => {
            steps.push(format!("--output {output}: adopt mode"));
            let d = adopt(output, &mut steps, &mut registry);
            return done(steps, d);
        }
        (Some(BackendId::Adopt), None) => {
            return done(
                steps,
                Decision::Unsupported("--backend adopt needs --output <name>".into()),
            );
        }
        (Some(BackendId::Hyprland), _) => {
            if req.output.is_some() {
                steps.push("--output is ignored with --backend hyprland".into());
            }
            let d = match hyprland_socket {
                Some(socket) => Decision::Hyprland { socket },
                None => Decision::Unsupported(
                    "--backend hyprland: no Hyprland IPC socket (is HYPRLAND_INSTANCE_SIGNATURE set? try --instance)".into(),
                ),
            };
            return done(steps, d);
        }
        (Some(BackendId::Wlroots), _) => {
            if req.output.is_some() {
                steps.push("--output is ignored with --backend wlroots".into());
            }
            let d = match &env.swaysock {
                Some(sock) => {
                    steps.push(format!("SWAYSOCK={sock}"));
                    sway(sock, &mut steps, &mut registry)
                }
                None => Decision::Unsupported(
                    "--backend wlroots: SWAYSOCK is not set (only Sway can create outputs for now)"
                        .into(),
                ),
            };
            return done(steps, d);
        }
        (Some(b @ (BackendId::Kde | BackendId::Portal)), _) => {
            return done(
                steps,
                Decision::Unsupported(format!("the {b} backend is not implemented yet ({ISSUE})")),
            );
        }
        (None, None) => {}
    }

    // 2. Hyprland. Never trust XDG_CURRENT_DESKTOP alone.
    if let Some(socket) = hyprland_socket {
        return done(steps, Decision::Hyprland { socket });
    }
    let looks_like_hyprland = env
        .current_desktop
        .as_deref()
        .is_some_and(|d| d.split(':').any(|p| p == "Hyprland"));
    let hyprland_hint = |m: String| {
        if looks_like_hyprland {
            format!(
                "{m}. XDG_CURRENT_DESKTOP says Hyprland but its IPC socket was not found: run od-sender from a terminal inside Hyprland, or pick an instance with --instance (see `od-sender doctor`)"
            )
        } else {
            m
        }
    };

    // 3. Sway.
    if let Some(sock) = &env.swaysock {
        steps.push(format!("SWAYSOCK={sock}"));
        let d = sway(sock, &mut steps, &mut registry);
        return done(steps, d);
    }

    // 4. A Wayland compositor that can capture and inject, but no IPC we know.
    if env.wayland_display.is_some() {
        if let Ok(s) = registry(&mut steps) {
            if has_capture(&s) && has_input(&s) {
                return done(
                    steps,
                    Decision::Unsupported(hyprland_hint(
                        "your compositor can capture and inject input, but od-sender cannot create a display on it; pass --output <name> to use an existing output".into(),
                    )),
                );
            }
            // 5. KDE.
            let kde = env.kde_full_session
                || env
                    .current_desktop
                    .as_deref()
                    .is_some_and(|d| d.split(':').any(|p| p == "KDE"));
            if kde {
                steps.push(format!(
                    "KDE session; zkde_screencast_unstable_v1 {}",
                    yes(s.has("zkde_screencast_unstable_v1"))
                ));
                return done(
                    steps,
                    Decision::Unsupported(format!("KDE Plasma is not supported yet ({ISSUE})")),
                );
            }
        }
    } else {
        steps.push("WAYLAND_DISPLAY not set".into());
    }

    // 6. Portal (GNOME and others): the D-Bus probe lands with the portal backend.
    if let Some(d) = &env.current_desktop {
        steps.push(format!("XDG_CURRENT_DESKTOP={d}"));
    }

    // 7. Nothing.
    if env.session_type.as_deref() == Some("x11") {
        return done(
            steps,
            Decision::Unsupported(
                "od-sender needs a Wayland session (XDG_SESSION_TYPE=x11)".into(),
            ),
        );
    }
    let desktop = env.current_desktop.as_deref().unwrap_or("unknown desktop");
    done(
        steps,
        Decision::Unsupported(hyprland_hint(format!(
            "no supported compositor found ({desktop}); od-sender supports Hyprland >= 0.55 for now ({ISSUE})"
        ))),
    )
}

/// Sway: its IPC socket must exist and the compositor must offer what the
/// wlroots backend uses.
fn sway(
    sock: &str,
    steps: &mut Vec<String>,
    registry: &mut dyn FnMut(&mut Vec<String>) -> Result<Snapshot, String>,
) -> Decision {
    let path = PathBuf::from(sock);
    if !path.exists() {
        return Decision::Unsupported(format!(
            "SWAYSOCK={sock} does not exist; run od-sender from a terminal inside Sway"
        ));
    }
    match registry(steps) {
        Ok(s) => {
            let missing = wlroots_missing(&s);
            if missing.is_empty() {
                Decision::Wlroots { swaysock: path }
            } else {
                Decision::Unsupported(format!(
                    "this Sway lacks {}; od-sender needs Sway >= 1.10",
                    missing.join(", ")
                ))
            }
        }
        Err(e) => Decision::Unsupported(format!("Sway: no Wayland connection ({e})")),
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Exit code for an unsupported desktop.
pub const EXIT_UNSUPPORTED: i32 = 2;

/// An unsupported desktop, carried as an error so `main` can exit with
/// [`EXIT_UNSUPPORTED`].
#[derive(Debug)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsupported {}

/// Turn a decision into a live backend (connects to Hyprland, checks its version).
pub fn instantiate(decision: Decision) -> Result<Box<dyn Backend>> {
    match decision {
        Decision::Hyprland { socket } => Ok(Box::new(HyprlandBackend::connect(socket)?)),
        Decision::Wlroots { swaysock } => Ok(Box::new(WlrootsBackend::connect(swaysock)?)),
        Decision::Adopt { output } => Ok(Box::new(AdoptBackend::new(output))),
        Decision::Unsupported(msg) => Err(anyhow!(Unsupported(msg))),
    }
}

/// Detect from the process environment.
pub fn detect(req: &Request) -> Result<Box<dyn Backend>> {
    let walk = decide(
        &Environment::from_process(),
        req,
        &mut crate::wayland::registry::snapshot,
    );
    for s in &walk.steps {
        tracing::debug!("detect: {s}");
    }
    instantiate(walk.decision)
}

/// Apply `--instance <sig|index>` to this process: point both the Hyprland
/// IPC and the Wayland connection at that instance. Must run before any
/// thread is spawned.
pub fn select_instance(sig_or_index: &str) -> Result<super::hyprland::instance::Instance> {
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") else {
        bail!("--instance: XDG_RUNTIME_DIR not set");
    };
    let inst = super::hyprland::instance::resolve(std::path::Path::new(&rt), sig_or_index)?;
    // SAFETY: called from `main` before the runtime or any other thread starts.
    unsafe {
        std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", &inst.signature);
        std::env::set_var("WAYLAND_DISPLAY", &inst.wl_socket);
    }
    Ok(inst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_globals() -> Snapshot {
        Snapshot {
            globals: [
                "wl_shm",
                "ext_image_copy_capture_manager_v1",
                "ext_output_image_capture_source_manager_v1",
                "zwlr_virtual_pointer_manager_v1",
            ]
            .iter()
            .map(|g| (g.to_string(), 1))
            .collect(),
            outputs: vec![],
        }
    }

    /// A runtime dir with a fake Hyprland socket path (a plain file: detection
    /// only checks that it exists).
    fn runtime_with_socket(tag: &str, sig: &str) -> PathBuf {
        let rt = std::env::temp_dir().join(format!("od-detect-{tag}-{}", std::process::id()));
        let socket = ipc::socket_path(&rt, sig);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::fs::write(&socket, b"").unwrap();
        rt
    }

    fn run(env: &Environment, req: &Request, globals: Option<Snapshot>) -> (Walk, u32) {
        let mut calls = 0;
        let walk = decide(env, req, &mut || {
            calls += 1;
            globals.clone().ok_or_else(|| anyhow!("connection refused"))
        });
        (walk, calls)
    }

    fn unsupported(w: &Walk) -> &str {
        match &w.decision {
            Decision::Unsupported(m) => m,
            d => panic!("expected unsupported, got {d:?}"),
        }
    }

    #[test]
    fn hyprland_with_a_live_socket_wins_without_probing_wayland() {
        let rt = runtime_with_socket("hypr", "sig_1_2");
        let env = Environment {
            runtime_dir: Some(rt.clone()),
            hyprland_signature: Some("sig_1_2".into()),
            wayland_display: Some("wayland-1".into()),
            current_desktop: Some("Hyprland".into()),
            ..Default::default()
        };
        let (w, calls) = run(&env, &Request::default(), Some(full_globals()));
        assert_eq!(
            w.decision,
            Decision::Hyprland {
                socket: ipc::socket_path(&rt, "sig_1_2")
            }
        );
        assert_eq!(calls, 0);
        let _ = std::fs::remove_dir_all(rt);
    }

    #[test]
    fn signature_without_socket_falls_through() {
        let env = Environment {
            runtime_dir: Some("/nonexistent".into()),
            hyprland_signature: Some("stale".into()),
            wayland_display: Some("wayland-1".into()),
            current_desktop: Some("Hyprland".into()),
            ..Default::default()
        };
        let (w, _) = run(&env, &Request::default(), Some(Snapshot::default()));
        assert!(
            w.steps.iter().any(|s| s.contains("missing")),
            "{:?}",
            w.steps
        );
        assert!(unsupported(&w).contains("no supported compositor"));
    }

    #[test]
    fn xdg_current_desktop_alone_is_not_hyprland() {
        let env = Environment {
            current_desktop: Some("Hyprland".into()),
            ..Default::default()
        };
        let (w, calls) = run(&env, &Request::default(), None);
        assert_eq!(calls, 0);
        assert!(unsupported(&w).contains("no supported compositor"));
        assert!(
            w.steps
                .iter()
                .any(|s| s == "HYPRLAND_INSTANCE_SIGNATURE not set")
        );
    }

    #[test]
    fn sway_selects_wlroots_when_it_has_the_protocols() {
        let sock = std::env::temp_dir().join(format!("od-detect-sway-{}.sock", std::process::id()));
        std::fs::write(&sock, b"").unwrap();
        let env = Environment {
            swaysock: Some(sock.to_string_lossy().into()),
            wayland_display: Some("wayland-1".into()),
            ..Default::default()
        };
        let mut globals = full_globals();
        globals.globals.push(("zwlr_output_manager_v1".into(), 4));
        let (w, probes) = run(&env, &Request::default(), Some(globals.clone()));
        assert_eq!(
            w.decision,
            Decision::Wlroots {
                swaysock: sock.clone()
            }
        );
        assert_eq!(probes, 1);

        // Sway 1.9: no ext-image-copy-capture, no output manager listed here.
        let mut old = globals.clone();
        old.globals
            .retain(|(g, _)| !g.starts_with("ext_") && g != "zwlr_output_manager_v1");
        let (w, _) = run(&env, &Request::default(), Some(old));
        let msg = unsupported(&w);
        assert!(msg.contains("ext_image_copy_capture_manager_v1"), "{msg}");
        assert!(msg.contains("zwlr_output_manager_v1"), "{msg}");
        assert!(msg.contains("Sway >= 1.10"), "{msg}");

        // An explicit --output still means adopt, even on Sway.
        let req = Request {
            backend: None,
            output: Some("HEADLESS-2".into()),
        };
        let (w, _) = run(&env, &req, Some(globals));
        assert_eq!(
            w.decision,
            Decision::Adopt {
                output: "HEADLESS-2".into()
            }
        );
        std::fs::remove_file(&sock).unwrap();
    }

    #[test]
    fn stale_swaysock_is_named() {
        let env = Environment {
            swaysock: Some("/nonexistent/sway-ipc.sock".into()),
            wayland_display: Some("wayland-1".into()),
            ..Default::default()
        };
        let (w, probes) = run(&env, &Request::default(), Some(full_globals()));
        assert!(unsupported(&w).contains("does not exist"));
        assert_eq!(probes, 0);
    }

    #[test]
    fn capable_unknown_compositor_suggests_adopt_and_output_selects_it() {
        let env = Environment {
            wayland_display: Some("wayland-0".into()),
            current_desktop: Some("niri".into()),
            ..Default::default()
        };
        let (w, calls) = run(&env, &Request::default(), Some(full_globals()));
        assert!(unsupported(&w).contains("--output <name>"));
        assert_eq!(calls, 1);

        let req = Request {
            output: Some("HEADLESS-1".into()),
            ..Default::default()
        };
        let (w, _) = run(&env, &req, Some(full_globals()));
        assert_eq!(
            w.decision,
            Decision::Adopt {
                output: "HEADLESS-1".into()
            }
        );
        // Adopt needs the protocols.
        let (w, _) = run(&env, &req, Some(Snapshot::default()));
        assert!(unsupported(&w).contains("lacks"));
    }

    #[test]
    fn output_beats_hyprland() {
        let rt = runtime_with_socket("adopt", "sig_3_4");
        let env = Environment {
            runtime_dir: Some(rt.clone()),
            hyprland_signature: Some("sig_3_4".into()),
            wayland_display: Some("wayland-1".into()),
            ..Default::default()
        };
        let req = Request {
            output: Some("eDP-1".into()),
            ..Default::default()
        };
        let (w, _) = run(&env, &req, Some(full_globals()));
        assert_eq!(
            w.decision,
            Decision::Adopt {
                output: "eDP-1".into()
            }
        );
        let _ = std::fs::remove_dir_all(rt);
    }

    #[test]
    fn forced_backends() {
        let env = Environment::default();
        let force = |b| Request {
            backend: Some(b),
            output: None,
        };
        let (w, _) = run(&env, &force(BackendId::Hyprland), None);
        assert!(unsupported(&w).contains("--instance"));
        let (w, _) = run(&env, &force(BackendId::Adopt), None);
        assert!(unsupported(&w).contains("needs --output"));
        let (w, _) = run(&env, &force(BackendId::Wlroots), None);
        assert!(unsupported(&w).contains("SWAYSOCK is not set"));
        let (w, _) = run(&env, &force(BackendId::Kde), None);
        assert!(unsupported(&w).contains("not implemented yet"));
    }

    #[test]
    fn kde_and_x11_are_named() {
        let env = Environment {
            wayland_display: Some("wayland-0".into()),
            current_desktop: Some("KDE".into()),
            ..Default::default()
        };
        let (w, _) = run(&env, &Request::default(), Some(Snapshot::default()));
        assert!(unsupported(&w).starts_with("KDE Plasma"));

        let env = Environment {
            session_type: Some("x11".into()),
            ..Default::default()
        };
        let (w, _) = run(&env, &Request::default(), None);
        assert!(unsupported(&w).contains("needs a Wayland session"));
    }
}
