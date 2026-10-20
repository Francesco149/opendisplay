mod backend;
mod desktop;
mod dev;
mod discover;
mod doctor;
mod encoder;
mod latest;
mod session;
mod wayland;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use clap::{Args as ClapArgs, Parser, Subcommand};
use tracing::{error, info};

use backend::detect::{self, Request};
use backend::{BackendId, Position};

/// OpenDisplay sender for Linux: extends a Wayland desktop onto an
/// OpenDisplay receiver.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Target one of several running Hyprland instances, like `hyprctl -i`
    /// (index from `od-sender doctor`, or the full signature). Sets
    /// HYPRLAND_INSTANCE_SIGNATURE and WAYLAND_DISPLAY for this process.
    #[arg(long, global = true, value_name = "SIG|INDEX")]
    instance: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Which compositor backend to use.
#[derive(ClapArgs, Debug, Clone, Default)]
pub struct BackendArgs {
    /// Force a backend instead of detecting one.
    #[arg(long, value_enum)]
    pub backend: Option<BackendId>,
    /// Use this existing output instead of creating one (adopt mode).
    #[arg(long, value_name = "NAME")]
    pub output: Option<String>,
}

impl BackendArgs {
    pub fn request(&self) -> Request {
        Request {
            backend: self.backend,
            output: self.output.clone(),
        }
    }
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Extend the desktop onto a receiver: dial it (or discover it), create a
    /// virtual output sized from its `hello`, capture, encode and stream.
    Run {
        /// Receiver: `host:port`, or a name/id to discover; omit for the
        /// first receiver found.
        #[arg(long, value_name = "NAME|ID|ADDR")]
        to: Option<String>,
        /// Receiver address (same as `--to host:port`).
        #[arg(long, hide = true, conflicts_with = "to")]
        connect: Option<SocketAddr>,
        /// Only accept a discovered receiver whose name contains this (same as `--to <name>`).
        #[arg(long, hide = true, conflicts_with_all = ["to", "connect"])]
        name: Option<String>,
        #[arg(long, default_value_t = 20.0)]
        bitrate_mbps: f32,
        /// Frame-rate cap; default: the receiver's `displayMaxFrameRate`
        /// (120 on ProMotion), never above it.
        #[arg(long)]
        max_fps: Option<f32>,
        /// Output scale: `passthrough` (the receiver's own scale), `mac`
        /// (2 for Retina-class panels, else 1, like the Mac sender) or a number.
        #[arg(long, default_value = "passthrough")]
        scale: desktop::ScalePolicy,
        /// H.264 encoder backend (#360).
        #[arg(long, value_enum, default_value_t = encoder::Backend::Auto)]
        encoder: encoder::Backend,
        /// Encoder threads (0 = all cores).
        #[arg(long, default_value_t = 0)]
        threads: u16,
        /// Do not bake the cursor into the video.
        #[arg(long)]
        no_cursor: bool,
        /// Position of the virtual output: auto-right, auto-left, auto-up, auto-down, or "x,y".
        #[arg(long, default_value = "auto-right")]
        position: Position,
        /// Leave the virtual output in place when the sender exits (development).
        #[arg(long)]
        keep_output: bool,
        #[arg(long)]
        no_reconnect: bool,
        /// Stop after this many seconds.
        #[arg(long)]
        duration: Option<f64>,
        #[command(flatten)]
        backend: BackendArgs,
    },
    /// List receivers on the local network.
    List {
        /// How long to browse, in seconds.
        #[arg(long, default_value_t = 3.0)]
        timeout: f64,
    },
    /// Explain what the sender would use here: backend, protocols, encoders.
    /// Exits non-zero on an unsupported desktop.
    Doctor {
        #[command(flatten)]
        backend: BackendArgs,
    },
    /// Remove virtual outputs left behind by senders that are no longer running.
    Clean {
        #[command(flatten)]
        backend: BackendArgs,
    },
    /// Developer self-tests.
    Dev {
        #[command(subcommand)]
        cmd: dev::Dev,
    },
}

/// Resolve `--to` (or the legacy `--connect`/`--name`) to an address.
fn resolve_receiver(
    to: Option<String>,
    connect: Option<SocketAddr>,
    name: Option<String>,
) -> Result<SocketAddr> {
    if let Some(a) = connect {
        return Ok(a);
    }
    let to = to.or(name);
    if let Some(a) = to.as_deref().and_then(|t| t.parse::<SocketAddr>().ok()) {
        return Ok(a);
    }
    let found = discover::find(to.as_deref(), Duration::from_secs(10))?;
    info!(
        "discovered {} at {} (pv {}, id {:?})",
        found.instance, found.addr, found.pv, found.id
    );
    Ok(found.addr)
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();
    let code = match real_main() {
        Ok(code) => code,
        Err(e) => {
            error!("{e:#}");
            if e.downcast_ref::<detect::Unsupported>().is_some() {
                detect::EXIT_UNSUPPORTED
            } else {
                1
            }
        }
    };
    std::process::exit(code);
}

fn real_main() -> Result<i32> {
    let args = Args::parse();
    // Before any thread exists: this rewrites the process environment.
    if let Some(i) = &args.instance {
        let inst = detect::select_instance(i)?;
        info!(
            "instance {} (pid {}, {})",
            inst.signature, inst.pid, inst.wl_socket
        );
    }
    match args.cmd {
        Cmd::Run {
            to,
            connect,
            name,
            bitrate_mbps,
            max_fps,
            scale,
            threads,
            encoder,
            no_cursor,
            position,
            keep_output,
            no_reconnect,
            duration,
            backend,
        } => {
            let mut backend = detect::detect(&backend.request())?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let addr = resolve_receiver(to, connect, name)?;
                session::run(
                    addr,
                    backend.as_mut(),
                    session::RunOptions {
                        bitrate_mbps,
                        desktop: desktop::DesktopOptions { scale, max_fps },
                        threads: dev::threads_or_all(threads),
                        encoder,
                        cursor_in_video: !no_cursor,
                        position,
                        keep_output,
                        reconnect: !no_reconnect,
                        duration: duration.map(Duration::from_secs_f64),
                    },
                )
                .await
            })?;
        }
        Cmd::List { timeout } => {
            let found = discover::list(Duration::from_secs_f64(timeout))?;
            if found.is_empty() {
                println!("no receivers found");
                return Ok(1);
            }
            for f in found {
                println!(
                    "{}\t{}\tpv {}\tid {}",
                    f.instance,
                    f.addr,
                    f.pv,
                    f.id.as_deref().unwrap_or("-")
                );
            }
        }
        Cmd::Doctor { backend } => {
            if !doctor::doctor(&backend.request()) {
                return Ok(detect::EXIT_UNSUPPORTED);
            }
        }
        Cmd::Clean { backend } => {
            let mut backend = detect::detect(&backend.request())?;
            let removed = backend.clean()?;
            if removed.is_empty() {
                info!("nothing to clean ({})", backend.describe());
            } else {
                info!("removed {}", removed.join(", "));
            }
        }
        Cmd::Dev { cmd } => dev::run(cmd)?,
    }
    Ok(0)
}
