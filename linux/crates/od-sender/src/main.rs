mod backend;
mod config;
mod desktop;
mod dev;
mod discover;
mod doctor;
mod encoder;
mod latest;
#[cfg(feature = "pipewire")]
mod pipewire;
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
    /// Config file; default `$XDG_CONFIG_HOME/opendisplay/sender.toml`.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<std::path::PathBuf>,
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
    /// Capture protocol: auto (wlr-screencopy on Hyprland < 0.57, else
    /// ext-image-copy-capture), ext or wlr [default: auto, or the config
    /// file's `capture`].
    #[arg(long, value_enum)]
    pub capture: Option<backend::capture::CaptureChoice>,
}

impl BackendArgs {
    /// The request, with the config file's `capture` under the flag.
    pub fn request_with(&self, config: Option<backend::capture::CaptureChoice>) -> Request {
        Request {
            backend: self.backend,
            output: self.output.clone(),
            capture: self.capture.or(config).unwrap_or_default(),
        }
    }

    pub fn request(&self) -> Request {
        self.request_with(None)
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
        /// Target bitrate [default: 20, or the config file's].
        #[arg(long)]
        bitrate_mbps: Option<f32>,
        /// Frame-rate cap; default: the receiver's `displayMaxFrameRate`
        /// (120 on ProMotion), never above it.
        #[arg(long)]
        max_fps: Option<f32>,
        /// Output scale: `passthrough` (the receiver's own scale), `mac`
        /// (2 for Retina-class panels, else 1, like the Mac sender) or a
        /// number [default: passthrough, or the config file's].
        #[arg(long)]
        scale: Option<desktop::ScalePolicy>,
        /// H.264 encoder backend (#360) [default: auto, or the config file's].
        #[arg(long, value_enum)]
        encoder: Option<encoder::Backend>,
        /// Encoder threads (0 = all cores).
        #[arg(long, default_value_t = 0)]
        threads: u16,
        /// Cursor delivery: `auto` (side channel when the receiver and
        /// compositor support it, else painted into the video), `video`, or
        /// `off` [default: auto, or the config file's].
        #[arg(long, value_enum)]
        cursor: Option<session::CursorMode>,
        /// Same as `--cursor off`.
        #[arg(long, conflicts_with = "cursor")]
        no_cursor: bool,
        /// Position of the virtual output: auto-right, auto-left, auto-up,
        /// auto-down, or "x,y" [default: auto-right, or the config file's].
        #[arg(long)]
        position: Option<Position>,
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

/// A receiver to dial; `id` and the Bonjour name only when discovered.
struct Receiver {
    addr: SocketAddr,
    id: Option<String>,
    name: String,
}

/// Resolve `--to` (or the legacy `--connect`/`--name`) to an address.
fn resolve_receiver(
    to: Option<String>,
    connect: Option<SocketAddr>,
    name: Option<String>,
) -> Result<Receiver> {
    let direct = |addr: SocketAddr| Receiver {
        addr,
        id: None,
        name: addr.to_string(),
    };
    if let Some(a) = connect {
        return Ok(direct(a));
    }
    let to = to.or(name);
    if let Some(a) = to.as_deref().and_then(|t| t.parse::<SocketAddr>().ok()) {
        return Ok(direct(a));
    }
    let found = discover::find(to.as_deref(), Duration::from_secs(10))?;
    info!(
        "discovered {} at {} (pv {}, id {:?})",
        found.instance, found.addr, found.pv, found.id
    );
    Ok(Receiver {
        addr: found.addr,
        id: found.id,
        name: found.instance,
    })
}

/// The config file: `--config` (must exist) or the default path (optional).
fn load_config(explicit: Option<&std::path::Path>) -> Result<config::Config> {
    match explicit {
        Some(p) => config::Config::load(p, true),
        None => match config::default_path() {
            Some(p) => config::Config::load(&p, false),
            None => Ok(config::Config::default()),
        },
    }
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
            cursor,
            no_cursor,
            position,
            keep_output,
            no_reconnect,
            duration,
            backend,
        } => {
            let config = load_config(args.config.as_deref())?;
            let cli = config::Layer {
                position,
                scale,
                bitrate_mbps,
            };
            let eff = cli.over(&config.defaults).over(&config::builtin());
            let to = to.or_else(|| {
                (connect.is_none() && name.is_none())
                    .then(|| config.to.clone())
                    .flatten()
            });
            let mut backend = detect::detect(&backend.request_with(config.capture))?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let receiver = resolve_receiver(to, connect, name)?;
                session::run(
                    receiver.addr,
                    backend.as_mut(),
                    session::RunOptions {
                        bitrate_mbps: eff.bitrate_mbps.unwrap_or(20.0),
                        desktop: desktop::DesktopOptions {
                            scale: eff.scale.unwrap_or_default(),
                            max_fps: max_fps.or(config.max_fps),
                        },
                        threads: dev::threads_or_all(threads),
                        encoder: encoder.or(config.encoder).unwrap_or(encoder::Backend::Auto),
                        cursor: if no_cursor {
                            session::CursorMode::Off
                        } else {
                            cursor.or(config.cursor).unwrap_or_default()
                        },
                        position: eff.position.unwrap_or(Position::AutoRight),
                        keep_output,
                        reconnect: !no_reconnect,
                        duration: duration.map(Duration::from_secs_f64),
                        receiver_id: receiver.id,
                        receiver_name: receiver.name,
                        cli,
                        config,
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
            let w = found
                .iter()
                .map(|f| f.instance.len())
                .max()
                .unwrap_or(4)
                .max(4);
            println!("{:w$}  {:21}  pv  id", "name", "address");
            for f in found {
                println!(
                    "{:w$}  {:21}  {:<2}  {}",
                    f.instance,
                    f.addr.to_string(),
                    f.pv,
                    f.id.as_deref().unwrap_or("-")
                );
            }
        }
        Cmd::Doctor { backend } => {
            // A broken config is reported by doctor itself.
            let capture = load_config(args.config.as_deref())
                .ok()
                .and_then(|c| c.capture);
            if !doctor::doctor(&backend.request_with(capture), args.config.as_deref()) {
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
