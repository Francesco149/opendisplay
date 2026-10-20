//! Decode + present. The protocol side hands us one access unit at a time
//! (§5: display in arrival order, latest wins); everything after that is a
//! GStreamer pipeline, chosen so the same binary picks hardware decode where
//! the machine has it (`vah264dec`, `v4l2slh264dec`) and software where not
//! (`avdec_h264`), and presents through `waylandsink` zero-copy when it can.

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    /// A GTK 4 window (`gtk4paintablesink`): fullscreens itself on the chosen
    /// monitor, draws the sender's cursor, sends pointer input back. The
    /// default when built with the `gtk` feature.
    Gtk,
    /// `glimagesink`: its own xdg toplevel via GStreamer GL; survives output
    /// hotplug. Default until the waylandsink bug below is fixed upstream.
    /// Fullscreen/output placement comes from a compositor window rule.
    Gl,
    /// `waylandsink`, fullscreen on a chosen output (`fullscreen-output`).
    /// GStreamer <= 1.28.7 crashes in `gstwldisplay.c:output_done` when the
    /// compositor re-sends `wl_output.done` (any output hotplug/move): it
    /// `g_object_steal_data`s the display pointer on the first `done`, so
    /// the second dereferences NULL. Opt-in until that is fixed.
    Wayland,
    /// `autovideosink`: whatever the machine has (debugging).
    Auto,
    /// `fakesink`: decode but do not present (CI, headless decode checks).
    Fake,
    /// No pipeline at all: protocol-only (CI without GStreamer).
    None,
}

impl std::str::FromStr for SinkKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "gtk" => SinkKind::Gtk,
            "gl" => SinkKind::Gl,
            "wayland" => SinkKind::Wayland,
            "auto" => SinkKind::Auto,
            "fake" => SinkKind::Fake,
            "none" => SinkKind::None,
            other => {
                return Err(format!(
                    "unknown sink `{other}` (gtk|gl|wayland|auto|fake|none)"
                ));
            }
        })
    }
}

impl std::fmt::Display for SinkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SinkKind::Gtk => "gtk",
            SinkKind::Gl => "gl",
            SinkKind::Wayland => "wayland",
            SinkKind::Auto => "auto",
            SinkKind::Fake => "fake",
            SinkKind::None => "none",
        })
    }
}

#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub sink: SinkKind,
    /// GStreamer decoder element; `decodebin3` autoplugs by rank.
    pub decoder: String,
    pub fullscreen: bool,
    /// `wl_output` name for `waylandsink fullscreen-output`.
    pub output: Option<String>,
    /// Insert `videoconvert` before the sink (see `launch_description`).
    pub videoconvert: bool,
    /// `Gtk` only: GDK can make a GL context, so frames go through
    /// `glsinkbin` (GL upload/convert, dmabuf import) instead of
    /// `videoconvert` into system memory.
    pub gl: bool,
}

pub trait VideoOutput: Send {
    /// Decode and present this access unit as soon as possible.
    fn push(&mut self, annexb: &[u8], is_idr: bool) -> Result<()>;
    /// Parameter sets changed or a new sender connected: drop everything buffered.
    fn reset(&mut self) -> Result<()>;
    /// Non-blocking: has the pipeline reported an error since the last call?
    /// Implementations recover internally; the caller requests a keyframe.
    fn poll_error(&mut self) -> Option<String>;
    /// Human-readable description of the decode/present path.
    fn describe(&mut self) -> String;
    /// Frames that actually reached the sink, when the backend can tell.
    fn rendered(&self) -> Option<u64> {
        None
    }
    /// The sender's cursor moved (§6.2, normalised to the video); presenters
    /// that draw no cursor ignore it.
    fn cursor(&mut self, _x: Option<f64>, _y: Option<f64>, _visible: bool) {}
    /// The sender's cursor sprite changed.
    fn cursor_image(&mut self, _img: &opendisplay_proto::control::CursorImg) {}
}

/// Counts frames and does nothing else.
#[derive(Default)]
pub struct NullOutput {
    pub frames: u64,
}

impl VideoOutput for NullOutput {
    fn push(&mut self, _annexb: &[u8], _is_idr: bool) -> Result<()> {
        self.frames += 1;
        Ok(())
    }
    fn reset(&mut self) -> Result<()> {
        Ok(())
    }
    fn poll_error(&mut self) -> Option<String> {
        None
    }
    fn describe(&mut self) -> String {
        "none".into()
    }
}

pub fn open(cfg: &VideoConfig) -> Result<Box<dyn VideoOutput>> {
    match cfg.sink {
        SinkKind::None => Ok(Box::new(NullOutput::default())),
        #[cfg(feature = "gstreamer")]
        _ => Ok(Box::new(gst_out::GstOutput::new(cfg)?)),
        #[cfg(not(feature = "gstreamer"))]
        _ => anyhow::bail!("built without the `gstreamer` feature; only --sink none is available"),
    }
}

#[cfg(feature = "gstreamer")]
pub mod gst_out {
    use super::*;
    use anyhow::{Context, anyhow};
    use gst::prelude::*;
    use gstreamer as gst;
    use gstreamer_app as gst_app;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tracing::{info, warn};

    pub struct GstOutput {
        cfg: VideoConfig,
        desc: String,
        pipeline: gst::Pipeline,
        appsrc: gst_app::AppSrc,
        decoder_name: Option<String>,
        rendered: Arc<AtomicU64>,
        /// Decoders demoted after failing here, for `describe`.
        demoted: Vec<String>,
        /// Push count at which "nothing rendered" means the decoder failed.
        stall_after: u64,
        /// A failure found outside the bus (a silent decoder), for `poll_error`.
        pending_error: Option<String>,
        generation: u64,
        pushed: u64,
        warned_stall: bool,
    }

    fn launch_description(cfg: &VideoConfig) -> String {
        let sink = match cfg.sink {
            SinkKind::Wayland => {
                let mut s = String::from("waylandsink sync=false");
                if cfg.fullscreen {
                    s.push_str(" fullscreen=true");
                    if let Some(o) = &cfg.output {
                        s.push_str(&format!(" fullscreen-output={o}"));
                    }
                }
                s
            }
            SinkKind::Gl => "glimagesink sync=false".into(),
            // The paintable sink itself is set on glsinkbin in `new`, so the
            // window can find it by name; without GL it converts on the CPU.
            SinkKind::Gtk if cfg.gl => "glsinkbin name=glsink sync=false".into(),
            SinkKind::Gtk => "gtk4paintablesink name=gtksink sync=false".into(),
            SinkKind::Auto => "autovideosink sync=false".into(),
            SinkKind::Fake => "fakesink sync=false".into(),
            SinkKind::None => unreachable!(),
        };
        // appsrc is unbounded and never drops: dropping *encoded* frames would
        // break the reference chain. Latest-wins happens after the decoder, in
        // the single-buffer leaky queue in front of the sink.
        //
        // videoconvert sits *after* the queue so only frames that will be shown
        // are converted; it is passthrough when the sink takes the decoder's
        // format directly. It is needed because waylandsink's shm path only
        // offers the RGB formats the compositor advertises (no I420/NV12 on
        // e.g. virtio-gpu), and software decoders output I420. On hardware
        // with a dmabuf-capable sink and VA decoder this element should be
        // bypassed (--no-videoconvert) to keep the zero-copy path.
        //
        // The identity element carries a pad probe that counts frames that
        // actually reach the sink, so "pushed" and "rendered" never get
        // conflated in stats again.
        let convert = if cfg.videoconvert && !(cfg.sink == SinkKind::Gtk && cfg.gl) {
            "! videoconvert "
        } else {
            ""
        };
        format!(
            "appsrc name=src is-live=true format=time do-timestamp=true block=false max-bytes=0 \
             caps=video/x-h264,stream-format=byte-stream,alignment=au \
             ! h264parse ! {decoder} \
             ! queue name=present max-size-buffers=1 max-size-bytes=0 max-size-time=0 leaky=downstream \
             {convert}! identity name=rendered silent=true ! {sink}",
            decoder = cfg.decoder,
        )
    }

    impl GstOutput {
        pub fn new(cfg: &VideoConfig) -> Result<Self> {
            gst::init().context("initialising GStreamer")?;
            #[cfg(feature = "gtk")]
            if cfg.sink == SinkKind::Gtk {
                gstgtk4::plugin_register_static().context("registering gtk4paintablesink")?;
            }
            for required in ["appsrc", "h264parse", "queue"] {
                if gst::ElementFactory::find(required).is_none() {
                    return Err(anyhow!(
                        "GStreamer element `{required}` missing (install gst-plugins-base / gst-plugins-bad)"
                    ));
                }
            }
            if cfg.decoder != "decodebin3" && gst::ElementFactory::find(&cfg.decoder).is_none() {
                return Err(anyhow!("decoder element `{}` not found", cfg.decoder));
            }
            if cfg.decoder == "decodebin3" && !has_h264_decoder() {
                return Err(anyhow!(
                    "no H.264 decoder element installed; install gst-libav (avdec_h264) or gst-plugin-va (vah264dec)"
                ));
            }
            if cfg.sink == SinkKind::Wayland {
                warn!(
                    "waylandsink <= 1.28.7 crashes on output hotplug (gstwldisplay.c output_done); --sink gl is the safe default"
                );
            }
            let rendered = Arc::new(AtomicU64::new(0));
            let (desc, pipeline, appsrc) = Self::build(cfg, &rendered)?;
            Ok(Self {
                cfg: cfg.clone(),
                desc,
                pipeline,
                appsrc,
                decoder_name: None,
                rendered,
                demoted: Vec::new(),
                // A working decoder shows its first frame within a few; a
                // quarter second at 60 FPS is plenty before giving up on it.
                stall_after: 15,
                pending_error: None,
                generation: 0,
                pushed: 0,
                warned_stall: false,
            })
        }

        /// A fresh pipeline for `cfg`, counting rendered frames into `rendered`.
        fn build(
            cfg: &VideoConfig,
            rendered: &Arc<AtomicU64>,
        ) -> Result<(String, gst::Pipeline, gst_app::AppSrc)> {
            let desc = launch_description(cfg);
            let pipeline = gst::parse::launch(&desc)
                .with_context(|| format!("building pipeline: {desc}"))?
                .downcast::<gst::Pipeline>()
                .map_err(|_| anyhow!("not a pipeline"))?;
            let appsrc = pipeline
                .by_name("src")
                .context("appsrc missing")?
                .downcast::<gst_app::AppSrc>()
                .map_err(|_| anyhow!("src is not an appsrc"))?;
            #[cfg(feature = "gtk")]
            if cfg.sink == SinkKind::Gtk && cfg.gl {
                let sink = gst::ElementFactory::make("gtk4paintablesink")
                    .name("gtksink")
                    .build()
                    .context("gtk4paintablesink")?;
                pipeline
                    .by_name("glsink")
                    .context("glsinkbin missing")?
                    .set_property("sink", &sink);
            }
            let counter = rendered.clone();
            let probe_pad = pipeline
                .by_name("rendered")
                .and_then(|e| e.static_pad("src"))
                .context("identity pad")?;
            probe_pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                counter.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });
            pipeline
                .set_state(gst::State::Playing)
                .context("starting pipeline")?;
            info!("pipeline: {desc}");
            Ok((desc, pipeline, appsrc))
        }

        /// Throw the pipeline away and build it again: decodebin3 keeps its
        /// decoder across state changes and caches the factory ranks it was
        /// created with, so only a new one plugs a different decoder.
        fn recreate(&mut self) -> Result<()> {
            let _ = self.pipeline.set_state(gst::State::Null);
            let (desc, pipeline, appsrc) = Self::build(&self.cfg, &self.rendered)?;
            self.desc = desc;
            self.pipeline = pipeline;
            self.appsrc = appsrc;
            self.decoder_name = None;
            self.generation += 1;
            Ok(())
        }

        /// Bumped whenever the pipeline (and so its sink) is replaced.
        pub fn generation(&self) -> u64 {
            self.generation
        }

        /// The `gtk4paintablesink`, whose `paintable` the window shows.
        pub fn paintable_sink(&self) -> Option<gst::Element> {
            let mut it = self.pipeline.iterate_recurse();
            while let Ok(Some(el)) = it.next() {
                if el.name() == "gtksink" {
                    return Some(el);
                }
            }
            None
        }

        fn rebuild(&mut self) -> Result<()> {
            warn!("rebuilding pipeline after error");
            self.pipeline
                .set_state(gst::State::Null)
                .context("stopping pipeline")?;
            self.pipeline
                .set_state(gst::State::Playing)
                .context("restarting pipeline")?;
            self.decoder_name = None;
            Ok(())
        }

        /// The factory of the video decoder decodebin3 plugged, if any.
        fn decoder_factory(&self) -> Option<gst::ElementFactory> {
            let mut it = self.pipeline.iterate_recurse();
            while let Ok(Some(el)) = it.next() {
                if let Some(f) = el.factory() {
                    let klass = f.metadata(gst::ELEMENT_METADATA_KLASS).unwrap_or("");
                    if klass.contains("Decoder") && klass.contains("Video") {
                        return Some(f);
                    }
                }
            }
            None
        }

        fn find_decoder(&mut self) -> Option<String> {
            if self.decoder_name.is_some() {
                return self.decoder_name.clone();
            }
            let mut it = self.pipeline.iterate_recurse();
            while let Ok(Some(el)) = it.next() {
                if let Some(f) = el.factory() {
                    let klass = f.metadata(gst::ELEMENT_METADATA_KLASS).unwrap_or("");
                    if klass.contains("Decoder") && klass.contains("Video") {
                        self.decoder_name = Some(f.name().to_string());
                        break;
                    }
                }
            }
            self.decoder_name.clone()
        }
    }

    fn has_h264_decoder() -> bool {
        let list = gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::DECODER,
            gst::Rank::MARGINAL,
        );
        list.iter().any(|f| {
            f.static_pad_templates().iter().any(|t| {
                t.direction() == gst::PadDirection::Sink
                    && t.caps().iter().any(|s| s.name() == "video/x-h264")
            })
        })
    }

    impl VideoOutput for GstOutput {
        fn push(&mut self, annexb: &[u8], is_idr: bool) -> Result<()> {
            let mut buffer = gst::Buffer::from_slice(annexb.to_vec());
            if !is_idr {
                buffer
                    .get_mut()
                    .unwrap()
                    .set_flags(gst::BufferFlags::DELTA_UNIT);
            }
            self.appsrc
                .push_buffer(buffer)
                .map_err(|e| anyhow!("appsrc push: {e:?}"))?;
            self.pushed += 1;
            // A pipeline that accepts frames but never shows one is a
            // negotiation problem hiding behind the leaky queue. Usually the
            // decoder: a V4L2 stateless one whose dmabuf the sink cannot take
            // drops every frame without an error. Demote it and rebuild so
            // decodebin3 plugs the next; say so once if that does not help.
            if self.pushed >= self.stall_after && self.rendered.load(Ordering::Relaxed) == 0 {
                let factory = self.decoder_factory();
                match factory {
                    Some(f) if f.rank() > gst::Rank::NONE => {
                        warn!(
                            "decoder {} showed no frame after {} pushed; falling back to the next one",
                            f.name(),
                            self.pushed
                        );
                        f.set_rank(gst::Rank::NONE);
                        self.demoted.push(f.name().to_string());
                        self.recreate()?;
                        self.stall_after = self.pushed + 60;
                        self.pending_error =
                            Some(format!("decoder {} produced no frames", f.name()));
                    }
                    _ if !self.warned_stall => {
                        self.warned_stall = true;
                        warn!(
                            "{} frames pushed, none reached the sink: check caps negotiation (GST_DEBUG=3)",
                            self.pushed
                        );
                    }
                    _ => {}
                }
            }
            Ok(())
        }

        fn reset(&mut self) -> Result<()> {
            // A flush drops everything queued in the parser, decoder and the
            // present queue; the next IDR (with SPS/PPS) restarts decoding.
            self.pipeline.send_event(gst::event::FlushStart::new());
            self.pipeline.send_event(gst::event::FlushStop::new(true));
            Ok(())
        }

        fn poll_error(&mut self) -> Option<String> {
            if let Some(e) = self.pending_error.take() {
                return Some(e);
            }
            let bus = self.pipeline.bus()?;
            let mut error = None;
            let mut failed_decoder = None;
            while let Some(msg) = bus.timed_pop_filtered(
                gst::ClockTime::ZERO,
                &[gst::MessageType::Error, gst::MessageType::Warning],
            ) {
                match msg.view() {
                    gst::MessageView::Error(e) => {
                        error = Some(format!("{} ({})", e.error(), e.debug().unwrap_or_default()));
                        failed_decoder = failed_decoder.or_else(|| self.decoder_factory());
                    }
                    gst::MessageView::Warning(w) => warn!(
                        "gstreamer: {} ({})",
                        w.error(),
                        w.debug().unwrap_or_default()
                    ),
                    _ => {}
                }
            }
            // A decoder that fails before a single frame was shown is not
            // going to work here (a V4L2 stateless decoder whose dmabuf the
            // sink cannot take, a broken VA driver): demote it so decodebin3
            // picks the next one on rebuild.
            if let (Some(_), Some(factory)) = (&error, failed_decoder) {
                if self.rendered.load(Ordering::Relaxed) == 0 && factory.rank() > gst::Rank::NONE {
                    warn!(
                        "decoder {} failed before showing a frame; falling back to the next one",
                        factory.name()
                    );
                    factory.set_rank(gst::Rank::NONE);
                    self.demoted.push(factory.name().to_string());
                    if let Err(e) = self.recreate() {
                        warn!("pipeline rebuild failed: {e:#}");
                    }
                    return error;
                }
            }
            if error.is_some() {
                if let Err(e) = self.rebuild() {
                    warn!("pipeline rebuild failed: {e:#}");
                }
            }
            error
        }

        fn describe(&mut self) -> String {
            let mut d = match self.find_decoder() {
                Some(d) => format!("{d} -> {}", self.desc.rsplit("! ").next().unwrap_or("?")),
                None => self.desc.clone(),
            };
            if !self.demoted.is_empty() {
                d.push_str(&format!(" (skipped {})", self.demoted.join(", ")));
            }
            d
        }

        fn rendered(&self) -> Option<u64> {
            Some(self.rendered.load(Ordering::Relaxed))
        }
    }

    impl Drop for GstOutput {
        fn drop(&mut self) {
            let _ = self.pipeline.set_state(gst::State::Null);
        }
    }
}
