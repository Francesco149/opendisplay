//! `od-sender dev …`: developer self-tests, kept out of the user-facing CLI.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use opendisplay_proto::control::{Scroll, Touch, TouchPhase};
use tracing::info;

use crate::backend::hyprland::ipc::HyprlandIpc;
use crate::backend::{CaptureEvent, CaptureOptions, DisplaySpec, FrameBuf, Position, detect};
use crate::encoder;
use crate::wayland::{capture, input};

#[derive(Subcommand, Debug)]
pub enum Dev {
    /// List Hyprland monitors over IPC.
    Monitors,
    /// Create, size and remove a headless output through Hyprland IPC.
    HeadlessTest {
        #[arg(long, default_value = "od-test")]
        name: String,
        #[arg(long, default_value_t = 2560)]
        width: u32,
        #[arg(long, default_value_t = 1600)]
        height: u32,
        #[arg(long, default_value_t = 60)]
        hz: u32,
        #[arg(long, default_value_t = 2.0)]
        scale: f64,
        /// Keep the output alive this long before removing it.
        #[arg(long, default_value_t = 2.0)]
        hold: f64,
    },
    /// Encode synthetic frames to measure software encode cost on this machine.
    EncodeBench {
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 800)]
        height: u32,
        #[arg(long, default_value_t = 60)]
        frames: u32,
        #[arg(long, default_value_t = 0)]
        threads: u16,
        #[arg(long, default_value_t = 20.0)]
        bitrate_mbps: f32,
        /// H.264 encoder backend (#360).
        #[arg(long, value_enum, default_value_t = encoder::Backend::Auto)]
        encoder: encoder::Backend,
    },
    /// Create a headless output, move the pointer onto it via the virtual
    /// pointer protocol and verify the position through Hyprland IPC.
    InputTest {
        #[arg(long, default_value_t = 0.25)]
        x: f64,
        #[arg(long, default_value_t = 0.75)]
        y: f64,
    },
    /// Capture an output via ext-image-copy-capture into shm and report frame timing.
    CaptureTest {
        /// wl_output name; default: first output.
        #[arg(long)]
        output: Option<String>,
        #[arg(long, default_value_t = 5.0)]
        seconds: f64,
        #[arg(long)]
        cursor: bool,
        /// Write the last frame as a PPM here.
        #[arg(long)]
        save: Option<std::path::PathBuf>,
    },
    /// Through the detected backend: open (or adopt) a display, capture and
    /// encode `--frames` frames while moving the pointer over it, scroll
    /// once, tear down. Exits non-zero on any failed step (SENDER-PLAN §4.3).
    Smoke {
        /// Display size, WxH.
        #[arg(long, default_value = "640x480")]
        size: String,
        #[arg(long, default_value_t = 30)]
        frames: u32,
        #[arg(long, default_value_t = 20.0)]
        timeout: f64,
        #[command(flatten)]
        backend: crate::BackendArgs,
    },
}

pub fn threads_or_all(threads: u16) -> u16 {
    if threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get() as u16)
            .unwrap_or(4)
    } else {
        threads
    }
}

pub fn run(cmd: Dev) -> Result<()> {
    match cmd {
        Dev::EncodeBench {
            width,
            height,
            frames,
            threads,
            bitrate_mbps,
            encoder: backend,
        } => encode_bench(width, height, frames, threads, bitrate_mbps, backend),
        Dev::InputTest { x, y } => input_test(x, y),
        Dev::Monitors => {
            let ipc = HyprlandIpc::from_env()?;
            info!("{}", ipc.version()?);
            for m in ipc.monitors()? {
                info!(
                    "{} {}x{} @{:.0} scale {} at {},{} ({})",
                    m.name, m.width, m.height, m.refresh_rate, m.scale, m.x, m.y, m.description
                );
            }
            Ok(())
        }
        Dev::HeadlessTest {
            name,
            width,
            height,
            hz,
            scale,
            hold,
        } => {
            let ipc = HyprlandIpc::from_env()?;
            let t = Instant::now();
            ipc.create_headless(&name)?;
            let m = ipc.configure(&name, width, height, hz, scale, Position::AutoRight)?;
            info!(
                "created {} as {}x{} @{:.0} scale {} at {},{} in {:?}",
                m.name,
                m.width,
                m.height,
                m.refresh_rate,
                m.scale,
                m.x,
                m.y,
                t.elapsed()
            );
            std::thread::sleep(Duration::from_secs_f64(hold));
            ipc.remove_output(&name)?;
            info!(
                "removed {name}; monitors now: {:?}",
                ipc.monitors()?
                    .iter()
                    .map(|m| m.name.clone())
                    .collect::<Vec<_>>()
            );
            Ok(())
        }
        Dev::CaptureTest {
            output,
            seconds,
            cursor,
            save,
        } => capture_test(output, seconds, cursor, save),
        Dev::Smoke {
            size,
            frames,
            timeout,
            backend,
        } => smoke(
            &backend.request(),
            &size,
            frames,
            Duration::from_secs_f64(timeout),
        ),
    }
}

fn encode_bench(
    width: u32,
    height: u32,
    frames: u32,
    threads: u16,
    bitrate_mbps: f32,
    backend: encoder::Backend,
) -> Result<()> {
    let threads = threads_or_all(threads);
    let mut enc = encoder::Encoder::new(
        width,
        height,
        encoder::EncoderSettings {
            backend,
            bitrate_bps: (bitrate_mbps * 1e6) as u32,
            max_fps: 60.0,
            threads,
        },
    )?;
    let stride = width * 4;
    let mut frame = crate::backend::Frame {
        captured_at: Instant::now(),
        presentation_ns: None,
        buf: FrameBuf::Cpu {
            pixels: vec![0u8; (stride * height) as usize],
            width,
            height,
            stride,
            format: crate::backend::PixelFormat::Xrgb8888,
        },
    };
    let mut times = Vec::new();
    let mut bytes = 0usize;
    for i in 0..frames {
        // Desktop-like content: text-ish noise blocks that shift each frame.
        if let FrameBuf::Cpu { pixels, .. } = &mut frame.buf {
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let v = (((x / 8 + y / 12 + i as usize) % 7) * 36) as u8;
                    let o = y * stride as usize + x * 4;
                    pixels[o] = v;
                    pixels[o + 1] = v / 2 + 60;
                    pixels[o + 2] = 255 - v;
                    pixels[o + 3] = 255;
                }
            }
        }
        let t = Instant::now();
        frame.captured_at = t;
        for e in enc.encode(&frame, i == 0)? {
            bytes += e.unit.annexb.len();
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let n = times.len();
    info!(
        "{} {width}x{height} {threads} threads: encode p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms -> {:.0} fps sustainable; avg frame {:.0} KB",
        enc.label(),
        times[n / 2],
        times[(n as f64 * 0.95) as usize],
        times[n - 1],
        1000.0 / times[n / 2],
        bytes as f64 / n as f64 / 1024.0
    );
    Ok(())
}

fn input_test(x: f64, y: f64) -> Result<()> {
    let ipc = HyprlandIpc::from_env()?;
    let name = "od-input-test";
    let before = ipc.request("cursorpos")?.trim().to_string();
    ipc.create_headless(name)?;
    let m = ipc.configure(name, 1600, 1000, 60, 2.0, Position::AutoRight)?;
    let injector = input::InputInjector::start(name, m.scale)?;
    injector.move_to(x, y);
    std::thread::sleep(Duration::from_millis(150));
    let after = ipc.request("cursorpos")?.trim().to_string();
    // Expected in Hyprland's layout coordinates: monitor origin + normalised * logical size.
    let (lw, lh) = (m.width as f64 / m.scale, m.height as f64 / m.scale);
    let expect = (m.x as f64 + x * lw, m.y as f64 + y * lh);
    info!(
        "cursorpos before: {before}; after move to ({x},{y}) on {name}: {after}; expected ~({:.0}, {:.0})",
        expect.0, expect.1
    );
    drop(injector);
    ipc.remove_output(name)?;
    info!(
        "removed {name}; cursorpos now: {}",
        ipc.request("cursorpos")?.trim()
    );
    Ok(())
}

fn capture_test(
    output: Option<String>,
    seconds: f64,
    cursor: bool,
    save: Option<std::path::PathBuf>,
) -> Result<()> {
    let mut last: Option<(capture::FrameInfo, Vec<u8>)> = None;
    let n = capture::capture_shm(
        output.as_deref(),
        cursor,
        Duration::from_secs_f64(seconds),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        |info: &capture::FrameInfo, pixels: Vec<u8>| {
            if save.is_some() {
                last = Some((*info, pixels));
            }
        },
    )?;
    info!("{n} frames captured");
    if let (Some(path), Some((info, pixels))) = (save, last) {
        let mut ppm = format!("P6\n{} {}\n255\n", info.width, info.height).into_bytes();
        for row in pixels.chunks(info.stride as usize) {
            for px in row[..(info.width * 4) as usize].chunks(4) {
                // XRGB8888 / ARGB8888 little-endian: B, G, R, X
                ppm.extend_from_slice(&[px[2], px[1], px[0]]);
            }
        }
        std::fs::write(&path, ppm)?;
        info!("wrote {}", path.display());
    }
    Ok(())
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once('x')
        .with_context(|| format!("size {s:?}: expected WxH"))?;
    Ok((w.parse()?, h.parse()?))
}

/// TODO(§4.3): `--connect <addr>` to stream to `od-receiver --sink none`
/// instead of encoding locally.
fn smoke(req: &detect::Request, size: &str, frames: u32, timeout: Duration) -> Result<()> {
    let (width, height) = parse_size(size)?;
    let mut backend = detect::detect(req)?;
    info!("smoke: backend {}", backend.describe());
    let spec = DisplaySpec {
        name: "od-smoke".into(),
        width,
        height,
        hz: 60,
        scale: 1.0,
        position: Position::AutoRight,
    };
    let mut vd = backend.open(&spec)?;
    info!("smoke: display {}", vd.info());

    let (tx, rx) = std::sync::mpsc::sync_channel::<CaptureEvent>(2);
    vd.start_capture(
        CaptureOptions { cursor: true },
        Box::new(move |ev| {
            let _ = tx.try_send(ev);
        }),
    )?;
    let mut input = vd.input()?;
    let mut enc: Option<encoder::Encoder> = None;
    let (mut captured, mut encoded) = (0u32, 0u32);
    let start = Instant::now();
    let mut step = 0u32;
    let result: Result<()> = (|| {
        while captured < frames {
            if start.elapsed() > timeout {
                bail!("only {captured}/{frames} frames within {timeout:?}");
            }
            // The cursor is painted into the frames, so motion is damage.
            step += 1;
            let t = step as f64 * 0.05;
            input.touch(&Touch {
                phase: TouchPhase::Moved,
                x: 0.5 + 0.3 * t.cos(),
                y: 0.5 + 0.3 * t.sin(),
                t: None,
            });
            match rx.recv_timeout(Duration::from_millis(30)) {
                Ok(CaptureEvent::Frame(f)) => {
                    captured += 1;
                    let (w, h) = f.dimensions();
                    let e = match &mut enc {
                        Some(e) => e,
                        None => enc.insert(encoder::Encoder::new(
                            w,
                            h,
                            encoder::EncoderSettings {
                                backend: encoder::Backend::Auto,
                                bitrate_bps: 8_000_000,
                                max_fps: 60.0,
                                threads: threads_or_all(0),
                            },
                        )?),
                    };
                    encoded += e.encode(&f, captured == 1)?.len() as u32;
                }
                Ok(CaptureEvent::Ended(why)) => bail!("capture ended: {why}"),
                Err(_) => {}
            }
        }
        input.scroll(&Scroll { dx: 0.0, dy: 40.0 });
        Ok(())
    })();
    drop(input);
    let closed = vd.close(false);
    result?;
    closed?;
    info!(
        "smoke ok: {captured} frames captured, {encoded} encoded ({}) in {:.1}s",
        enc.as_ref().map_or("-", |e| e.label()),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
