//! `od-sender doctor`: what the sender would use on this desktop and why
//! (SENDER-PLAN §3.3). Read-only: it creates no output and takes no lock.

use std::path::PathBuf;

use crate::backend::detect::{self, Decision, Environment, Request};
use crate::backend::hyprland::{self, instance, ipc::HyprlandIpc};
use crate::backend::lock;
use crate::encoder;
use crate::wayland::registry::{self, INTERESTING};

/// Prints the report; returns `false` on an unsupported desktop.
pub fn doctor(req: &Request) -> bool {
    println!("od-sender {}", env!("CARGO_PKG_VERSION"));
    let env = Environment::from_process();

    let snapshot = env.wayland_display.as_ref().map(|_| registry::snapshot());
    let walk = detect::decide(&env, req, &mut || match &snapshot {
        Some(Ok(s)) => Ok(s.clone()),
        Some(Err(e)) => Err(anyhow::anyhow!("{e:#}")),
        None => Err(anyhow::anyhow!("WAYLAND_DISPLAY not set")),
    });

    println!("\ndetection:");
    for s in &walk.steps {
        println!("  {s}");
    }
    let mut supported = match &walk.decision {
        Decision::Hyprland { socket } => {
            println!("  => hyprland ({})", socket.display());
            hyprland_report(socket.clone())
        }
        Decision::Adopt { output } => {
            println!("  => adopt output {output}");
            true
        }
        Decision::Unsupported(m) => {
            println!("  => unsupported: {m}");
            false
        }
    };
    if supported {
        match detect::instantiate(walk.decision.clone()) {
            Ok(b) => println!("  {}: {}\n  {}", b.id(), b.describe(), b.caps()),
            Err(e) => {
                println!("  cannot start the backend: {e:#}");
                supported = false;
            }
        }
    }

    println!(
        "\nwayland ({}):",
        env.wayland_display.as_deref().unwrap_or("unset")
    );
    match &snapshot {
        Some(Ok(s)) => {
            for g in INTERESTING {
                match s.globals.iter().find(|(i, _)| i == g) {
                    Some((_, v)) => println!("  {g} v{v}"),
                    None => println!("  {g}: absent"),
                }
            }
            for o in &s.outputs {
                println!(
                    "  output {} {}x{} @{:.2} scale {} at {},{}",
                    o.name,
                    o.width,
                    o.height,
                    o.refresh_mhz as f64 / 1000.0,
                    o.scale,
                    o.x,
                    o.y
                );
            }
        }
        Some(Err(e)) => println!("  cannot connect: {e:#}"),
        None => println!("  not connected"),
    }

    if let Some(rt) = &env.runtime_dir {
        if let Ok(all) = instance::list(rt) {
            if !all.is_empty() {
                println!("\nhyprland instances (--instance <index|signature>):");
                for (i, x) in all.iter().enumerate() {
                    let current = env.hyprland_signature.as_deref() == Some(&x.signature);
                    println!(
                        "  {i}: {} pid {} {}{}",
                        x.signature,
                        x.pid,
                        x.wl_socket,
                        if current { " (selected)" } else { "" }
                    );
                }
            }
        }
    }

    println!("\nencoders:");
    for b in [
        encoder::Backend::V4l2m2m,
        encoder::Backend::X264,
        encoder::Backend::Openh264,
    ] {
        let s = encoder::EncoderSettings {
            backend: b,
            bitrate_bps: 2_000_000,
            max_fps: 60.0,
            threads: 1,
        };
        match encoder::Encoder::new(320, 240, s) {
            Ok(e) => println!("  {}: ok", e.label()),
            Err(e) => println!("  {b:?}: unavailable ({e:#})"),
        }
    }

    let dir: PathBuf = lock::dir();
    println!("\nlocks: {}", dir.display());
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            println!("  {}", e.file_name().to_string_lossy());
        }
    }
    supported
}

fn hyprland_report(socket: PathBuf) -> bool {
    let ipc = HyprlandIpc::new(socket.clone());
    match ipc.version_info() {
        Ok(v) => {
            let ok = v
                .triple()
                .is_some_and(|(maj, min, _)| (maj, min) >= hyprland::MIN_VERSION);
            println!(
                "  version {} ({})",
                v.version,
                match v.triple() {
                    Some((a, b, _)) if (a, b) < hyprland::MIN_VERSION => "too old: needs 0.55+",
                    Some((a, b, _)) if (a, b) > hyprland::TESTED_UP_TO => "newer than tested",
                    Some(_) => "tested",
                    None => "unparseable",
                }
            );
            match ipc.option("ecosystem:enforce_permissions") {
                Ok(o) if o.get("bool").and_then(|b| b.as_bool()) == Some(true) => println!(
                    "  ecosystem:enforce_permissions = true: allow screencopy for od-sender with an hl.permission rule, or it streams black frames"
                ),
                Ok(_) => println!("  ecosystem:enforce_permissions = false"),
                Err(e) => println!("  ecosystem:enforce_permissions: {e:#}"),
            }
            if let Ok(ms) = ipc.monitors() {
                for m in ms {
                    println!(
                        "  monitor {} {}x{} @{:.2} scale {} at {},{}",
                        m.name, m.width, m.height, m.refresh_rate, m.scale, m.x, m.y
                    );
                }
            }
            ok
        }
        Err(e) => {
            println!("  IPC error: {e:#}");
            false
        }
    }
}
