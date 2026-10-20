# Linux sender: plan to a public beta

Status: **working plan**, updated 2026-10-20 (§2 has the dated state).
Branch `linux-plan`, PR #298,
issue #299. Scope: `od-sender` only. History, research and the receiver are
in `PLAN.md` (§3.5, §3.6, §3.9, §4 M3); this file supersedes the sender
parts of §4 M3/M4/M6 and is the working plan from here.

Test bench: one machine, the user's own desktop, an M1 Pro on Asahi Linux with
Omarchy 4.0.3 (Arch, Hyprland 0.56.2, aquamarine 0.15.1), aarch64, no hardware
H.264 encoder (x264 via the system FFmpeg 9). Receivers on the LAN: iPhone
(iOS app), Android (Google Play beta app), 2017 Intel iMac running the Mac
receiver app. A crash costs the user their session; §4 says how to test
without that risk, and every live step is one sender at a time, stopped with
SIGINT (see `AGENTS.md`, `HANDOFF.md` §7).

Verification legend used below: *verified* = read from the installed
binaries/config or the tagged upstream source; *plausible* = documented or
in common use elsewhere, not tried here; *unverified* = assumption.

---

## 1. Goal and what "beta" means

Ship an `od-sender` that an Omarchy user installs from the AUR and uses daily
to extend their Hyprland desktop onto a phone, tablet or spare Mac, with the
Mac sender's core behaviour (virtual display sized from `hello`, touch and
scroll, rotation, reconnect) and without endangering their compositor. Beta
means the supported matrix is narrow and stated, not that the code is rough.

### 1.1 Support matrix at beta

| Tier | Setup | Meaning |
|---|---|---|
| **Tested** | Arch/Omarchy, Hyprland 0.56.x, aarch64 (Asahi, M1 Pro), software x264 encode; receivers: iPhone (App Store), Mac receiver app (Intel iMac) | Ran live on this machine |
| **Expected to work** | Arch/Omarchy on x86_64 (x264; VA-API not used yet); any distro with Hyprland >= 0.55 and an FFmpeg with libx264 (Fedora's `ffmpeg-free`: bundled OpenH264, slower); iPad; Android (Play beta, never run, §5 step 5) | Same code, untested hardware or receiver |
| **Experimental: verified headless only** | **Sway >= 1.10** (`backend/wlroots`); **KDE Plasma >= 6.8** (`backend/kde`; <= 6.7 only with the packaged `od-sender.desktop`, §8.12); **GNOME, Mutter >= 50** (`backend/gnome`; older Mutter sizes the monitor from the PipeWire format, untested). Each passes its CI smoke on a headless compositor (§4.3), never a real desktop session | Say "experimental" on the website; ask for reports |
| **Adopt only** | niri, COSMIC, river, labwc, Wayfire: `--output <name>` on an output you created, if the compositor offers ext-image-copy-capture (or wlr-screencopy) and `zwlr_virtual_pointer` | No output creation; unchecked per compositor |
| **Unsupported** | X11 sessions, Hyprland < 0.55 (pre-Lua IPC), Flatpak (no portal backend), USB/usbmuxd transport, HEVC, Apple Pencil pressure (mapped to a pointer), audio, mirror mode | `od-sender` exits with a message that names the gap and links the issue |

### 1.2 Exit criteria (all on this machine unless noted)

1. `od-sender run` to the iPhone, Android and Mac receivers: picture, touch,
   scroll, rotation rebuild, lock/unlock (`sleeping`) and app quit
   (`closing`) behave; reconnect after Wi-Fi loss within 10 s.
2. Thirty minutes of continuous use on each receiver without a Hyprland
   crash, orphaned `od-*` output, or stuck pointer button. The destructive
   checks (`kill -9`, double sender, reconfigure under capture) pass against
   a **nested** Hyprland (§4.1), never the real session.
3. `od-sender doctor` explains, on this machine, exactly what it will use
   (backend, protocols, encoder, permission state) and, on a shell with
   `HYPRLAND_INSTANCE_SIGNATURE` unset, refuses with a useful message.
4. A GitHub release carries `x86_64` and `aarch64` artifacts built by CI, and
   `yay -S opendisplay-sender` installs on Omarchy from the AUR; CI has built
   the PKGBUILD in an Arch container.
5. The website has a "Linux sender (beta)" card; `README.md` and
   `linux/README.md` tell a user how to install, run, and report.
6. PR #298 is merged (or the sender split out into a merged PR) so the beta
   is built from `main`.
7. The iMac latency question (§5 step 6) has an answer recorded, even if the
   fix is deferred.

Not required for beta: cursor side channel, hardware encode, other
compositors, Flatpak, HEVC.

---

## 2. Where the sender stands

*Status 2026-10-20.* Done: the trait split (§3) with backends for Hyprland
(IPC headless outputs), wlroots/Sway (IPC create/unplug,
`zwlr_output_manager_v1` sizing), KDE (zkde_screencast virtual output, KWin
fake input, PipeWire frames, scale/position through
`kde_output_management_v2`) and GNOME (Mutter's ScreenCast/RemoteDesktop
D-Bus APIs, PipeWire frames), plus adopt mode; capture through
ext-image-copy-capture or wlr-screencopy (`--capture`, auto picks
wlr-screencopy on Hyprland < 0.57: 40/40 output removals under 4K capture
survived on nested 0.56.2, ext crashed at the 7th, §8.13), zero-copy shm leases and banded
BGRA→I420 ahead of x264; desktop policy from `hello.panel` with a 300 ms
`hello` debounce; config file, `doctor`, `list`, `clean`, `--instance`;
one-sender-per-receiver lock, redial backoff, recreation of a removed
output, orderly teardown on SIGINT/SIGTERM/SIGHUP and on panics; cursor side
channel (off on Hyprland, §8.14); release workflow with static FFmpeg
tarballs; split Arch packages. Live: iPhone at ~19-21 ms capture-to-arrival
p50; the iMac's 220 ms was the sender's 4K software encode plus an
oldest-frame handoff bug, now e2e p50 55-67 ms (PLAN.md M3). CI: rust on
x86_64/aarch64, the Arch package with a headless Sway smoke, kde-smoke
(green, required) and gnome-smoke (experimental). Not yet: Android, a real (non-headless)
Sway/KDE/GNOME session, x86_64 hardware, VA-API, a published release or AUR
package, the website card.

Hazards (both Hyprland 0.56.2, both handled structurally): output
reconfigure/removal under an in-flight capture frame (`CapturedDisplay`
stops and joins capture first; wlr-screencopy for capture, §8.13), and the
cursor capture session (§8.14). Upstream drafts: `linux/research/`.

---

## 3. Architecture for compositor portability

The protocol, desktop policy, encoder, session loop, discovery, CLI and
config are compositor-independent. Only four things are not: creating a
virtual output, getting its frames, getting its cursor, and injecting input.
Those four go behind traits; a backend is a bundle of implementations chosen
at startup. The receiver already follows this shape (`VideoOutput` in
`od-receiver/src/video.rs`).

### 3.1 Module split

```
od-sender/src/
  main.rs              CLI (clap): run, list, doctor, clean, dev …
  config.rs            $XDG_CONFIG_HOME/opendisplay/sender.toml merged under CLI flags
  desktop.rs           hello -> DesktopPlan (panel/maxEncode/videoCaps/level limits), §3.6
  session.rs           today's run.rs: dial, SenderSession, capture->encode->wire; talks to traits only
  discover.rs          as today
  encoder/             as today (+ vaapi.rs later); input is a Frame, not &[u8]
  backend/
    mod.rs             traits below + BackendId + Caps
    detect.rs          startup probing and the unsupported-desktop messages, §3.3
    lock.rs            today's lock_output(): one sender per output name
    adopt.rs           "use an existing output": no create/remove, capture + input only (§3.7)
    hyprland/          today's hyprland.rs (IPC) + HyprlandDisplay (VirtualDisplay impl)
    wlroots/           Sway IPC create/unplug, zwlr_output_manager_v1 sizing, lock-side record; shares wayland/*
    kde/               zkde_screencast_unstable_v1 virtual output + fake input (landed); frames via src/pipewire.rs
    gnome/             Mutter ScreenCast.RecordVirtual + RemoteDesktop over D-Bus (no dialog), DisplayConfig for scale/position; frames via src/pipewire.rs
    portal/            later, Flatpak only: ScreenCast(VIRTUAL)+RemoteDesktop portals, PipeWire remote fd
  wayland/
    registry.rs        one connection + registry snapshot shared by probe, capture, input, cursor
    capture.rs         today's capture.rs: ext-image-copy-capture shm (dmabuf later) -> Frame
    input.rs           today's input.rs: zwlr_virtual_pointer -> InputSink impl
    cursor.rs          later: ext-image-copy-capture cursor session -> CursorSource impl
  pipewire.rs          later: PipeWire stream -> Frame, shared by kde/ and portal/
```

`wayland/*` is shared by every Wayland-protocol backend (Hyprland now, Sway,
adopt mode on niri/COSMIC/river later); `backend/*` holds only what differs
per compositor. The portal and KDE backends use `pipewire.rs` instead of
`wayland/capture.rs`.

### 3.2 Traits (what each owns)

```rust
// backend/mod.rs
pub enum BackendId { Hyprland, Wlroots, Kde, Portal, Adopt }

/// What the session asks for, derived from `hello` by desktop.rs.
pub struct DisplaySpec {
    pub name: String,            // "od-<6 chars of hello.id>"; also the lock key
    pub width: u32, pub height: u32, pub hz: u32, pub scale: f64,
    pub position: Position,      // AutoRight | AutoLeft | AutoUp | AutoDown | At(i32, i32)
}
pub struct DisplayInfo { pub name: String, pub width: u32, pub height: u32, pub scale: f64, pub x: i32, pub y: i32 }

pub struct Caps {
    pub cursor_metadata: bool,   // cursor delivered separately (ext cursor session / portal METADATA)
    pub dmabuf: bool,            // capture can hand out GPU buffers
    pub resize_in_place: bool,   // reconfigure without destroy+create (Hyprland: yes)
    pub creates_outputs: bool,   // false for Adopt
}

pub trait Backend: Send {
    fn id(&self) -> BackendId;
    fn describe(&self) -> String;                           // "Hyprland 0.56.2 (IPC socket …)" for logs/doctor
    fn caps(&self) -> Caps;
    /// Create (or adopt an unlocked leftover) and size the virtual output.
    fn open(&mut self, spec: &DisplaySpec) -> Result<Box<dyn VirtualDisplay>>;
    /// Remove `od-*` outputs that no running sender holds a lock on.
    fn clean(&mut self) -> Result<Vec<String>>;
}

/// One virtual display. Dropping it stops capture *then* removes the output.
pub trait VirtualDisplay: Send {
    fn info(&self) -> &DisplayInfo;
    fn start_capture(&mut self, opts: CaptureOptions, sink: FrameSink) -> Result<()>; // owns its thread
    fn stop_capture(&mut self);                             // joins; always precedes reconfigure/close
    fn reconfigure(&mut self, spec: &DisplaySpec) -> Result<()>; // stop_capture() first, enforced inside
    fn input(&mut self) -> Result<Box<dyn InputSink>>;
    fn cursor(&mut self) -> Option<Box<dyn CursorSource>>;  // None => bake the cursor into the video
    fn close(self: Box<Self>, keep_output: bool) -> Result<()>;
}

pub struct Frame { pub captured_at: Instant, pub presentation_ns: Option<u128>, pub buf: FrameBuf }
pub enum FrameBuf {
    Cpu { pixels: Vec<u8>, width: u32, height: u32, stride: u32, format: PixelFormat }, // BGRX/BGRA today
    Dmabuf { planes: Vec<DmabufPlane>, fourcc: u32, modifier: u64, width: u32, height: u32 }, // later
}
pub type FrameSink = Box<dyn FnMut(Frame) + Send>;

pub trait InputSink: Send {
    fn touch(&mut self, t: &Touch);
    fn scroll(&mut self, s: &Scroll);
    fn pencil(&mut self, p: &Pencil);                       // pointer mapping until a tablet protocol exists
}

pub trait CursorSource: Send {
    fn start(&mut self, sink: Box<dyn FnMut(CursorEvent) + Send>) -> Result<()>;
}
pub enum CursorEvent { Moved { x: f64, y: f64 }, Hidden, Image { png: Vec<u8>, nw: u32, nh: u32, ax: f64, ay: f64 } }
```

Why one `VirtualDisplay` object instead of separate output/capture/input
traits: on the portal the virtual monitor *is* the ScreenCast stream (one
session creates both), on KDE the `stream_virtual_output` request creates the
output and the PipeWire feed in one call, and on Hyprland the ordering "stop
capture before touching the output" is a crash rule, which a single object
with a `Drop` can enforce. The session loop never sees a `HyprlandIpc` again.

Per backend:

| | Hyprland (beta) | Adopt (next) | wlroots/Sway (landed) | KDE (landed) | Portal: GNOME (later) |
|---|---|---|---|---|---|
| Create/size output | IPC `output create headless`, `eval hl.monitor({...})`, verify via `j/monitors` (*verified*) | none: user names an output they created or a real one to mirror | Sway IPC `create_output` (Sway picks `HEADLESS-<n>`; found by diffing `get_outputs`), then mode, scale and position through `zwlr_output_manager_v1` (`set_custom_mode`), read back via `get_outputs` (*verified*, Sway 1.12, CI) | `zkde_screencast_unstable_v1.stream_virtual_output_with_description(name, description, logical w, h, scale)` (since v4; v2 without description) creates the output and returns a PipeWire node (*verified* in the protocol XML; whether unprivileged clients may bind it: *unverified*). Portal fallback: `SelectSources types=VIRTUAL` exists, but the **user picks the size in KDE's chooser dialog** (default 1920x1080, restorable via token `virtualSize`), the app cannot request it (*verified* in xdg-desktop-portal-kde `screencast.cpp`) | `ScreenCast.SelectSources` with `types = VIRTUAL`; size control *unverified* |
| Frames | `wayland/capture.rs` shm; dmabuf later | same | same | PipeWire | PipeWire |
| Cursor | baked in at beta; later ext cursor session (Hyprland implements it) | same | same protocol where implemented | PipeWire cursor metadata | `cursor_mode = METADATA` |
| Input | `wayland/input.rs` virtual pointer bound to the output | same, bound to that output | same | RemoteDesktop portal `NotifyPointerMotionAbsolute(stream, x, y)` (what the third-party sender does, §4.4) | RemoteDesktop portal |
| Remove output | IPC `output remove` | nothing | Sway IPC `output <name> unplug` (*verified*) | close the stream | close the session |

### 3.3 Backend detection at startup

`backend::detect(force: Option<BackendId>) -> Result<Box<dyn Backend>>`, in
this order, stopping at the first hit:

1. `--backend`/config override; `--output <name>` selects Adopt on any
   compositor that passes step 4's probe.
2. `HYPRLAND_INSTANCE_SIGNATURE` set and `$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock`
   exists → Hyprland. Then `version` over IPC: refuse below 0.55 (the Lua
   `eval` path does not exist), warn above the tested range. Do not trust
   `XDG_CURRENT_DESKTOP` alone (it is `Hyprland` here, but it is user-editable
   and absent in some launchers). `--instance <sig|index>` overrides the env
   for nested instances (§4.1).
3. `SWAYSOCK` set → wlroots/Sway, when the socket exists and the registry
   has ext-image-copy-capture, `zwlr_virtual_pointer_manager_v1` and
   `zwlr_output_manager_v1`; otherwise "this Sway lacks …; od-sender needs
   Sway >= 1.10", exit 2. The lock key stays `od-<id>`; a record next to it
   (`od-<id>.wlroots`: output name + `SWAYSOCK`) says which `HEADLESS-<n>` it
   owns, so a restart reuses it and `clean` unplugs only recorded outputs of
   dead senders.
4. Wayland registry probe (`wayland/registry.rs`, one roundtrip): if
   `ext_image_copy_capture_manager_v1` (or `zwlr_screencopy_manager_v1`) and
   `zwlr_virtual_pointer_manager_v1` exist but no known output-creating IPC
   is found (niri, COSMIC, river, labwc, wayfire) → without `--output`:
   "your compositor can capture and inject input, but od-sender cannot create
   a display on it; pass `--output <name>`"; with it → Adopt.
5. `XDG_CURRENT_DESKTOP` contains `KDE` or `KDE_FULL_SESSION` set and the
   registry has `zkde_screencast_unstable_v1` v2+ and `org_kde_kwin_fake_input`
   v3+ → KDE (`--backend kde` forces it). Without them: "KWin does not offer
   … (Plasma < 6.8: install the package's .desktop entry)".
6. D-Bus `org.freedesktop.portal.Desktop`: `ScreenCast.AvailableSourceTypes`
   has bit `VIRTUAL` (4) and `RemoteDesktop` exists → Portal. Skip this
   probe when step 2 matched: xdg-desktop-portal-hyprland advertises
   `VIRTUAL` without implementing it (PLAN.md §2.3).
7. `XDG_SESSION_TYPE=x11` → "od-sender needs a Wayland session". Otherwise
   "no supported compositor found" plus the list of what was probed.

`od-sender doctor` prints the same walk with every probe result, the Wayland
globals seen, the encoder probe (`libx264` present in this FFmpeg? V4L2
devices?), the Hyprland permission state (§8.7), and the lock directory, and
exits non-zero on an unsupported desktop. This is the first thing to ask a
bug reporter for.

### 3.4 Mapping today's files; refactor now vs later

Now (before beta, step 1 in §5):

| Today | Becomes | Change |
|---|---|---|
| `hyprland.rs` | `backend/hyprland/ipc.rs` + `backend/hyprland/mod.rs` | IPC unchanged; add `Backend`/`VirtualDisplay` impls wrapping today's `Display::create`/`stop` from `run.rs`; `--instance` |
| `run.rs` `Display`, `lock_output` | `backend/hyprland/display.rs`, `backend/lock.rs` | moved, behaviour identical |
| `run.rs` session loop | `session.rs` | takes `&mut dyn Backend`; `SenderAction::Hello` → `desktop::plan` → `open`/`reconfigure` |
| `capture.rs` | `wayland/capture.rs` | emits `Frame` with `FrameBuf::Cpu`; registry code moves to `wayland/registry.rs` |
| `input.rs` | `wayland/input.rs` | implements `InputSink`; add `pencil` mapping |
| `encoder/*` | unchanged | `encode(&Frame, force_idr)`; `Cpu` only, `Dmabuf` returns an error until VA-API lands |
| `main.rs` test subcommands (`monitors`, `headless-test`, `input-test`, `capture-test`) | `od-sender dev <...>` + `dev smoke` (§4.3) | kept, moved under one subcommand so the user-facing CLI stays small |
| (new) | `backend/adopt.rs` | ~50 lines: `open` looks the output up by name, no create/remove; used by `dev smoke` and CI first, user-facing after beta (§3.7) |

Later: `wayland/cursor.rs` (ext cursor session), `FrameBuf::Dmabuf` with
`zwp_linux_dmabuf_v1` allocation, `backend/wlroots`, `backend/kde`,
`backend/portal`, `pipewire.rs`. None of those touch `session.rs` again;
that is the test of the split.

### 3.5 Encoder side

The encoder already sits behind one shape (`encoder::Encoder`, enum dispatch
over FFmpeg and OpenH264). Keep it; add `Kind::Vaapi` (`h264_vaapi`, DRM
hwframes, dmabuf import) ahead of x264 in the auto order once an Intel/AMD box
is available. Nothing in it is compositor-specific.

**The FFmpeg soname problem.** `od-sender` links `libavcodec.so.63`,
`libavutil.so.61`, `libswscale.so.10` (*verified* with `readelf -d` on the
release binary). Arch bumps FFmpeg's major roughly yearly, so a prebuilt
binary dies at the next bump until repackaged; and CI's Ubuntu 24.04 runners
link FFmpeg 6.1 (`libavcodec.so.60`), so **today's CI binaries would not
even start on Arch**. Options:

| Option | Effect | Cost |
|---|---|---|
| A. Source-built AUR package (what the PKGBUILD already is) | Links whatever FFmpeg is installed; AUR helpers flag rebuilds after a bump (`rebuild-detector`) | Compile time for the user (~2-4 min); no cross-distro artifact |
| B. `-bin` AUR package from CI tarballs | One-liner install | Breaks on every FFmpeg soname bump until repackaged; CI must build on Arch (container) rather than Ubuntu to match sonames at all |
| C. Static FFmpeg: `ffmpeg-sys-next` features `build`, `build-lib-x264`, `build-license-gpl` (present in 9.0.0), only `avcodec`+`swscale` enabled | Binary depends on glibc, libx264.so.165 (stable since 2020; or static too), later libva.so.2/libdrm.so.2 (stable for years) → portable tarball for any distro, `-bin` package viable | Longer CI (FFmpeg from source, cacheable), we own FFmpeg security updates for the encode wrappers we use, GPL build (fine: project is GPL-3) |
| D. Drop FFmpeg, bind libx264/libva/V4L2 directly | Smallest deps | Weeks of work; `x264` crate is stale |

Recommendation: **A for the beta** (Omarchy is the target and `yay` builds
from source there anyway, including on aarch64 where the AUR has no binaries;
Omarchy's own `omarchy-pkg-aur-install` wraps `yay`), **C right after** so
the website can offer a generic tarball and a `-bin` package. B is a trap.
Keep `--no-default-features` style escape hatches out of the sender: OpenH264
is already the floor when FFmpeg lacks libx264.

*Status 2026-10-20:* C landed for the release tarballs.
`linux/packaging/ffmpeg/build-static.sh` builds a minimal static FFmpeg
n9.0.2 (encoders `libx264` + `h264_v4l2m2m` only) and a static x264 r3222;
`linux-release.yml` links od-sender against it via `FFMPEG_DIR` and fails on
any NEEDED library beyond glibc/libgcc/libstdc++ and libpipewire, and on a
hard glibc/libstdc++ symbol need above the promised floor
(`packaging/check-symbol-floor.sh`: glibc 2.38 on x86_64, 2.39 on aarch64,
GLIBCXX_3.4.32). Stripped (release profile `strip = "symbols"`; measured
2026-10-20 on CI artifacts, with the KDE and GNOME backends): 10.3 MB on
aarch64, 12.4 MB on x86_64, encode cost unchanged within noise. Both arches
build in CI. The AUR package (A) still links
the system FFmpeg.

### 3.6 Desktop policy (shared layer, needed at beta)

`desktop.rs` turns `hello` into a `DesktopPlan` the way `Mac/DesktopPolicy.swift`
does, so the Linux sender makes the same choices as the Mac sender:

* Prefer `hello.panel` (PROTOCOL §6.7) when valid; else the deprecated
  `pixelsWide/High/scale`. `opendisplay-proto::Hello` has no `panel` field
  yet; add it (and `power`, ignored).
* Output scale: `panel.scale >= 1.5` → 2 (Hyprland accepts fractional and 3,
  but matching the Mac sender's 1x/2x choice keeps text sizes familiar; make
  `--scale` an override; today's code passes `hello.scale` straight through,
  which gave iPhone sessions scale 3 — decide, see §8.3).
* Stream raster: `min(panel pixels, maxEncodeWide/High, videoCaps h264 entry,
  High@L5.2 frame-size and macroblock-rate limits)`; `framesPerSecond =
  min(displayMaxFrameRate, --max-fps)`. When the stream is smaller than the
  panel, size the **output** to the stream (capture stays 1:1, receiver
  upscales once), as §6.5 allows and the Mac sender does.
* Announce the result in `streamConfig` (already sent) and log the plan.
* Never pick HEVC (no encoder on this platform; §6.5 requires an affirmative
  offer anyway).

### 3.7 Adopt mode: use an output that already exists

`od-sender run --output <name>` skips output creation: it locks, captures and
injects on an output the user made (`swaymsg create_output`, `hyprctl output
create headless`, niri/COSMIC equivalents where they exist) or on a real
output (mirror; `panel` then only bounds the stream, PROTOCOL §6.7). Zero
compositor-specific code, so it is the cheapest breadth win: every compositor
that passes detection step 4 works on day one, and it is the vehicle for CI
smoke tests (§4.3) and for the nested-Hyprland harness. It lands internally
at beta (step 1) and as a documented user mode first after beta (§6).

---

## 4. Test bench: nested compositors and CI, not the user's desktop

### 4.1 A sacrificial Hyprland on this machine (nested)

Hyprland 0.56.2 requests three aquamarine backends: headless (mandatory),
DRM (if available) and Wayland (fallback) (*verified*: `src/Compositor.cpp`
v0.56.2 lines 311-318). The Wayland backend calls `wl_display_connect(nullptr)`,
i.e. it attaches to whatever `WAYLAND_DISPLAY` names (*verified*: aquamarine
v0.15.1 `src/backend/Wayland.cpp`). So `Hyprland -c <nested.lua>` started
from a terminal inside the running session comes up as a window of the real
session; the DRM backend fails because the seat is already taken, and the
nested instance gets its GPU allocator through the parent (*inferred* from
the sources; the Hyprland project describes aquamarine as running "inside a
Wayland compositor window", but this has not been launched here). Fully
headless Hyprland without any GPU or parent does **not** work: `CBackend::start`
needs a backend with a DRM fd to create the GBM allocator and otherwise fails
with "no allocator available"; `Compositor.cpp` still carries `// TODO:
headless only` (*verified* from both sources). There is no `--headless`
flag (`Hyprland --help`).

How the sender targets the nested instance: every instance is listed by
`hyprctl -j instances` with its `instance` signature and `wl_socket`
(here: one instance, signature `efb5…_1791974227_723898458`, `wayland-1`);
`hyprctl -i <sig|index>` addresses one. `od-sender` reads
`HYPRLAND_INSTANCE_SIGNATURE` (IPC) and `WAYLAND_DISPLAY` (capture, input),
so either run it from a terminal spawned *inside* the nested Hyprland (it
inherits both), or from the outer shell as
`HYPRLAND_INSTANCE_SIGNATURE=<nested sig> WAYLAND_DISPLAY=<nested wl_socket> od-sender run …`.
Step 1 adds `--instance <sig|index>` that resolves the pair the way
`hyprctl -i` does (directories under `$XDG_RUNTIME_DIR/hypr/`) and sets
both for the process, so a typo cannot land a test on the real session.

Harness (`linux/testbench/`): `nested-hyprland.lua` (no Omarchy includes, no
autostart, one `hl.monitor` for the window, `ecosystem.enforce_permissions`
left off), `nested.sh` that starts `Hyprland -c` in the background, polls
`hyprctl -j instances` for the new signature, prints the two variables, and
kills the instance on exit. Far end: `od-receiver --sink none --listen
127.0.0.1:9100` (exists) or the iPhone. Everything destructive runs there:
`kill -9` of the sender, a second sender on the same output, output
reconfiguration under capture, and the repro for the upstream
`CScreenshareFrame::transform` report. Worst case is a dead window.
*Verified 2026-10-20 (step 2 done):* `testbench/nested.sh` brings up a
nested 0.56.2 as a window (`wayland-2`); `od-sender run` against it plus
`od-receiver --sink none` on loopback created `od-3f31ed` in the nested
instance only, captured and encoded (x264, IDR delivered). On the nested
instance: `kill -9` of the sender leaves the output and the next start
reuses it; an `hl.monitor` mode change under capture and `hyprctl output
remove` under capture both left Hyprland running (so the Oct 6 abort needs
more than either alone; the double-sender case is still untested). Bug
found: after an external `output remove` the sender keeps the connection
with no output and no video; step 1 must surface "output lost" and
recreate. Caveats: the nested compositor's
headless outputs are captured through the same protocols, so the test is
representative for IPC, capture and input, but not for DRM timing.

### 4.2 Other compositors on this machine, later

Sway (`extra`): `WLR_BACKENDS=wayland sway -c testbench/sway.conf` nests as
a window (*plausible*, standard wlroots feature); `swaymsg create_output`
then gives a headless output inside it. GNOME and KDE: no nested mode that
carries a working portal (*unverified*); use an aarch64 QEMU VM on this box
(the earlier dev VM precedent) with GNOME or Plasma, networking bridged for
the phone or port-forwarded for `od-receiver --sink none` on the host.

### 4.3 CI: one headless job per backend on GitHub runners

Shape: `od-sender dev smoke --backend <x> [--output <name>] --connect
127.0.0.1:9100 --size 640x480 --frames 30` opens (or adopts) a display,
captures 30 frames, injects one touch and one scroll, tears down, and exits
non-zero on any step; the far end is `od-receiver --sink none` whose
`stats` counts frames and reports `streamConfig`/IDR. Each job starts the
compositor under `dbus-run-session`, runs the smoke with a 60 s timeout, and
uploads both logs. Jobs are gated on `paths` for their backend directory.

| Compositor | How | Status | Notes |
|---|---|---|---|
| **Sway, headless** | `archlinux:latest` container on `ubuntu-24.04` (x86_64): `pacman -S sway pixman …`, `WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c ci.conf &`, `swaymsg create_output`, `swaymsg output HEADLESS-2 mode --custom 640x480@60Hz`, then `dev smoke --output HEADLESS-2` (Adopt) and, once the backend exists, `--backend wlroots` | **verified** (sway 1.12, aarch64, 2026-10-20) with `linux/testbench/sway-smoke.sh`: `capture-test` + `input-test --output` pass; CI step in `linux.yml` `arch-package` (first run pending) | Ubuntu 24.04's sway 1.9 predates `ext-image-copy-capture` (sway >= 1.10), hence the Arch container, which also matches the AUR target and lets the same job run `makepkg` on the PKGBUILD (exit criterion 4). Exercises `wayland/capture.rs` + `wayland/input.rs` **at beta**, before any Sway backend |
| **Hyprland, headless** | — | *not feasible* on GPU-less hosted runners (§4.1: allocator needs a DRM fd) | The Hyprland backend's IPC layer gets unit tests against recorded `hyprctl` JSON instead; live coverage is the nested harness here |
| **Mutter (GNOME), headless** | `archlinux:latest` container: `mutter --headless --wayland --no-x11 --virtual-monitor 1280x800` under its own session bus, PipeWire and `wireplumber --profile policy`, **no system bus** (`DBUS_SYSTEM_BUS_ADDRESS` points nowhere), then `doctor`, `dev smoke --backend gnome` (1170x2532), scale 3→2 with `--position auto-left`, and `kill -9` cleanup; the run is under `strace` and fails on any system-bus connect | **verified locally** (Mutter 51.1, aarch64, `linux/testbench/gnome-smoke.sh`); CI job `gnome-smoke` experimental: first run failed before Mutter (no vgem on the runner kernel), render node now optional | Uses Mutter's private D-Bus APIs, not the portal: no chooser dialog, so it is automatable (`linux/research/gnome-backend.md`) |
| **KWin, virtual** | `kwin_wayland --virtual` with PipeWire running, then `dev smoke --backend kde` via `zkde_screencast_unstable_v1.stream_virtual_output` (no chooser dialog on that path) | **verified locally** (KWin 6.8.0 headless on aarch64 with a GPU render node, `linux/testbench/kde-smoke.sh`); CI job `kde-smoke` in `linux.yml` **green** and required since 2026-10-20, with the `vgem` module as render node on GPU-less runners (one runner kernel, 6.17.0-1022-azure, had no vgem: a run landing there skips the smoke and the job passes **without coverage**; its step summary says so and the separate, non-required `kde coverage` check turns red) | See the KDE findings below |

**Sway headless findings** (*verified* on sway 1.12 / wlroots 0.20, pixman
renderer, headless backend, no GPU, isolated `XDG_RUNTIME_DIR`; they feed the
future wlroots backend):

* Globals: `ext_image_copy_capture_manager_v1` v1 and
  `ext_output_image_capture_source_manager_v1` v1 (so `capture.rs` works
  unchanged; `zwlr_screencopy_manager_v1` v3 is there too, unused),
  `zwlr_virtual_pointer_manager_v1` **v2** (so `create_virtual_pointer_with_output`
  binds motion to our output), `zwp_virtual_keyboard_manager_v1` v1,
  `zwlr_output_manager_v1` v4, `zwlr_output_power_manager_v1`,
  `ext_foreign_toplevel_image_capture_source_manager_v1`.
* Outputs: the headless backend starts with `HEADLESS-1`; `swaymsg
  create_output` adds the next one at 1920x1080 with refresh 0. Names are
  never reused (after unplugging `HEADLESS-3` the next is `HEADLESS-4`), so
  find the new output by diffing `get_outputs`, never by guessing.
  `swaymsg output <name> unplug` removes it. `swaymsg -- output <name> mode
  --custom 640x480@60Hz` needs the `--` (swaymsg otherwise parses `--custom`).
* `zwlr_output_manager_v1` applies custom modes, scale and position to a
  headless output (`wlr-randr --custom-mode 1024x768@75Hz --scale 2 --pos
  0,800` took effect), so a wlroots backend can size its output through the
  protocol instead of compositor IPC; only creating and removing it needs
  `swaymsg`.
* Capture yields frames on damage only: a static output gives one frame in
  3 s; with a terminal redrawing, 180-183 frames in 3 s at 60 Hz (p50 16.4 ms)
  at 640x480 Xrgb8888. Changing the mode mid-capture produced one
  `BufferConstraints` failure, which `capture.rs` recovered from; unplugging
  the output mid-capture left Sway running (unlike the Hyprland
  reconfigure-under-capture crash, §5 step 4).
* Input: absolute motion from the output-bound virtual pointer lands at the
  right normalised position (checked through focus-follows-mouse on two
  side-by-side windows). Sway ignores the very first pointer motion after it
  starts for focus, so the smoke test warms up once.
* `input-test --output <name>` (added for this) adopts an existing output on
  any compositor with the virtual pointer protocol; without it the command
  still needs Hyprland IPC to create its output and read `cursorpos`.

**KWin findings** (KWin 6.8.0 from source and live on a headless
`kwin_wayland --virtual`, isolated D-Bus/PipeWire/WirePlumber):

* Globals for a plain client: `zkde_screencast_unstable_v1` v6,
  `org_kde_kwin_fake_input` v6, `kde_output_management_v2`, `zxdg_output_manager_v1`;
  **no** ext-image-copy-capture and no `zwlr_virtual_pointer`, so KDE cannot
  share `wayland/capture.rs`/`input.rs` or `CapturedDisplay`.
* `stream_virtual_output*` needs OpenGL compositing ("Unsupported compositing
  type" otherwise). The output is `Virtual-<name>`, 60 Hz fixed, and lives
  exactly as long as the stream object: KWin removes it on `close` or when
  the client dies (`kill -9` verified), so there is nothing to `clean` and
  `--keep-output` cannot work.
* Size: the protocol says logical; the virtual backend makes the mode
  `size * scale`, the 6.8 DRM backend uses the request as the mode. The
  backend requests logical and corrects once from the read-back (*verified*
  on the virtual backend; DRM path from source).
* Scale: KWin ignores the requested scale for new outputs and picks one from
  DPI (`OutputConfigurationStore::chooseScale`), unless the user saved one
  for that output name; `kscreen-doctor output.Virtual-od-<id>.scale.2` once
  made the next stream for that receiver come up at scale 2 (*verified*).
  The backend therefore sets scale and position itself through
  `kde_output_management_v2` (devices from `kde_output_device_registry_v2`
  on KWin >= 6.5, per-device globals before), right after creating the output.
* Position: KWin refuses negative coordinates ("Position of enabled output
  … is negative"), so `auto-left`/`auto-up` put the new output at 0,0 and
  shift the other outputs right/down in the same configuration, as KDE's
  display settings do; on close the remaining outputs move back so the
  layout starts at 0,0 again (KWin itself leaves them shifted). All five
  `--position` forms *verified*; after a `kill -9` KWin restores the
  layout itself (§8.17).
* PipeWire: the consumer must offer the output size as the preferred
  `VideoSize`, or KWin resamples (it negotiated 1920x1080 for 800x600).
  memfd/shm buffers when no modifiers are offered; BGRx.
* Fake input: `pointer_motion_absolute` in global logical coordinates moved
  the pointer onto the virtual output (KWin's `activeOutputName` over D-Bus).
* Results: `dev smoke --backend kde` 240 frames encoded at ~57 fps;
  `run --backend kde` into `od-receiver --sink none`: 1024x768 stream.

### 4.4 What the third-party Linux sender teaches (`tixwho/opendisplay-linux`)

Read on 2026-10-20 (branch `linux-port`, GPL-3.0, "vibe-coded Linux port",
C++/Qt6/Kirigami, x86_64-only PKGBUILD, last push 2026-08-02). Same licence
as this repo, so ideas and even code may be reused with attribution, though
the language makes direct reuse moot. Findings:

* **KDE** = generic portals + libkscreen: `RemoteDesktop.CreateSession` +
  `SelectDevices POINTER` + `ScreenCast.SelectSources types=VIRTUAL
  cursor_mode=EMBEDDED` + `Start` (chooser dialog on first run, restore
  token after), then it polls KScreen for the output that appeared and
  applies a **custom mode** (`KScreen::Output::Capability::CustomModes`,
  needs Plasma/libkscreen >= 6.6; widths rounded to multiples of 8 for CVT)
  and a position; input is `NotifyPointerMotionAbsolute(stream, x, y)`,
  `NotifyPointerButton`, `NotifyPointerAxis`. It restores the user's output
  positions on teardown. This is the fallback path for our KDE column; the
  zkde protocol path above avoids the dialog and the libkscreen dependency.
* **Hyprland** = `hyprctl output create headless` + `hl.monitor` (with a
  `keyword monitor` fallback for < 0.55) and then the **xdph ScreenCast
  portal with the user picking the new output in the share dialog**, frames
  over PipeWire, input via `zwlr_virtual_pointer_v1`. Our direct
  `ext-image-copy-capture` path has no dialog and no PipeWire hop. Worth
  copying: it pins the reference monitor's geometry while the headless
  output exists so Hyprland `auto` rules do not reorder outputs, and reloads
  the user's config on teardown; we should check whether `auto-right`
  misplaces anything when the user has `auto` rules (step 3).
* Scale: derives the virtual output's scale from physical DPI of both panels
  when known, rounded to 0.05, and nudges pixel sizes to integer logical
  sizes. Input for §8.3.
* Transport: Avahi (needs the daemon, which Omarchy ships inactive;
  `mdns-sd` avoids that) and **usbmuxd USB** via libusbmuxd — a reference
  if USB ever enters scope.
* Encoders: FFmpeg VA-API/NVENC/software; no cursor side channel; no
  `panel`/`maxEncode` handling visible.

---

## 5. Work plan to beta

*Status 2026-10-20:*

| Step | State | Left |
|---|---|---|
| 1 Trait split | done | — |
| 2 Nested harness | done (`testbench/nested.sh`) | — |
| 3 Protocol catch-up | done: `hello.panel`/`power`, `desktop.rs`, 300 ms debounce | `pencil` → pointer mapping (no-op today); verbatim `hello` log for fixtures |
| 4 Safety hardening | done: drop order, panics, SIGHUP/SIGTERM, lock per receiver, redial backoff, version gate, `doctor` permission check, removed-output recreation; verified on the nested instance | startup `clean` (a command today); `auto-right` against `auto` monitor rules |
| 5 Android receiver | not started | all |
| 6 iMac latency | answered (PLAN.md M3: encode-bound; 55-67 ms p50 after fixes) | hardware encode (#360) |
| 7 UX | done: `--to`, `list`, `doctor`, `clean`, `dev`, config file, desktop entry, user unit | — |
| 8 CI: Sway + PKGBUILD | done (`arch-package` job green) | mutation check once |
| 9 Release pipeline | workflow green on PRs and manual runs; static FFmpeg tarballs | first `linux-v*` tag |
| 10 AUR package | PKGBUILD split (`opendisplay-sender`) built in CI | publish to the AUR; `makepkg -si`/`namcap` here |
| 11 Website and docs | `linux/README.md` and the card on `linux/site-beta` | merge it |
| 12 Beta checklist | — | all |

Each step is small enough to finish in a sitting and ends with a check on this
machine. Order matters: the refactor comes first so every later step lands in
its final place, and the harness comes second so the destructive steps never
touch the real session.

1. **Trait split (no behaviour change).** Implement §3.1-3.4 for Hyprland
   plus `backend/adopt.rs`; `session.rs` compiles against `dyn Backend`;
   `od-sender doctor`, `clean`, `--instance`, `dev smoke`. Unit tests:
   detection on synthetic environments (env vars + fake socket path), lock
   semantics (exists), `desktop::plan` against recorded `hello`s, Hyprland
   IPC parsing against recorded `j/monitors` JSON. *Verify:* `cargo
   test/clippy/fmt`; one iPhone session with stats within noise of today's
   (`sent … fps`, `capture->send p50`); `env -u HYPRLAND_INSTANCE_SIGNATURE
   od-sender doctor` refuses cleanly.
2. **Nested-Hyprland harness (§4.1).** `linux/testbench/nested-hyprland.lua`,
   `nested.sh`; first launch of a nested instance; `dev smoke --instance
   <nested>` against `od-receiver --sink none`. *Verify:* the smoke passes
   inside the nested window; `hyprctl -j instances` shows two instances
   while it runs and one after; the real session is untouched.
3. **Protocol catch-up.** `Hello.panel` (+ `power`), `desktop.rs` (§3.6),
   `hello` debounce of 300 ms before a rebuild (§6.1; also a safety feature:
   fewer output reconfigurations), `pencil` → pointer mapping, log every
   `hello` verbatim at `info` once (we need the real Android and Mac `hello`s
   as fixtures). *Verify:* iPhone rotation still rebuilds once; the Mac
   receiver's 5K `hello` plans a 4096x2304 stream and output; unit tests
   with the three recorded `hello`s.
4. **Safety hardening, tested on the nested instance.** `VirtualDisplay`
   drop order enforced; panic in any thread → orderly teardown (drop guard,
   `panic = "unwind"` kept); SIGHUP/SIGPIPE handled like SIGINT; refuse to
   start when another sender holds the lock for the same receiver `id`;
   `clean` removes unlocked `od-*` outputs on startup; Hyprland version
   gate; `doctor` reads `ecosystem:enforce_permissions` and warns (§8.7);
   check `auto-right` against a config with `auto` rules (§4.4); keep
   `keep_output` as a documented dev flag. *Verify (nested only):* `kill -9`
   mid-stream → nested Hyprland alive, `od-*` output still listed, next
   `od-sender run --instance …` adopts and reconfigures it without a crash;
   second sender for the same phone exits with the lock message; SIGINT
   during the first second (before capture started) tears down cleanly;
   reconfigure-under-capture repro recorded for the upstream report.
5. **Android receiver.** First session against the Play beta app. Record its
   `hello` (does it send `panel`? `displayMaxFrameRate`? `cursorPort`?),
   check touch/scroll sign and units (§7), rotation re-`hello`, `sleeping`
   on screen-off, `closing` on quit, keyframe recovery after airplane-mode
   toggle. File anything receiver-side as issues against the Android app.
   *Verify:* exit criterion 1 for Android.
6. **iMac 220 ms investigation** (time-boxed to a few hours; record the
   answer in PLAN.md M3 status). The Mac receiver computes `e2e = (now +
   offset) - cap` at frame arrival with `offset` from the min-RTT ping/pong
   sample (`Shared/StreamReceiver.swift:819-825, 1408-1412`) and reads the
   socket with `minimumIncompleteLength: 1`, so receiver-side batching is
   out. Steps, cheapest first:
   a. Read the sender's own `capture->send p50` for a 4096x2304 session; run
      `od-sender dev encode-bench --width 4096 --height 2304`. If encode +
      BGRA→I420 is ~40 ms, the remaining ~180 ms is after the socket.
   b. Same session with `--max-fps 30` and with `--bitrate-mbps 8`: if e2e
      tracks bitrate, the iMac's link is the bottleneck (check its Wi-Fi band
      and `netstat -an` Recv-Q on the iMac; try Ethernet).
   c. Mac sender → iMac receiver on the same network as a baseline; if it
      also shows >100 ms, the receiver or network owns it, not `od-sender`.
   d. Compare the iMac's `PHONE-STATS` `rtt`, `offsetKnown`, `dec50`, `ph50`
      with the iPhone's; a wrong `offset` would show as e2e ≠ photon − decode.
   e. Only then suspect the sender's `cap` stamping (`session.rs`: `cap_wall
      = now - captured_at.elapsed()`; `captured_at` is taken in the capture
      callback, after the compositor's copy).
   Not a beta blocker if the picture and input are correct and the felt lag
   is tolerable; document it as a known issue otherwise.
7. **User-facing UX (modest).**
   * CLI: `od-sender run [--to <name|id|addr>]` (discovery with a 10 s
     timeout, or dial), `od-sender list` (receivers on the LAN with name,
     device, pv, address), `doctor`, `clean`, `dev <...>`. `--position`,
     `--bitrate-mbps`, `--max-fps`, `--encoder`, `--scale`, `--no-cursor`,
     `--instance` stay.
   * Config `~/.config/opendisplay/sender.toml`: defaults for the flags plus
     `[receiver."<id>"]` overrides (position, scale, bitrate) so a phone that
     always sits on the left stays there. CLI beats config.
   * Packaging extras: `od-sender.desktop` (`Terminal=true`, `Exec=od-sender
     run`) so it shows in the Omarchy menu, and a `systemd --user` template
     `od-sender@.service` (`ExecStart=od-sender run --to %i`,
     `PartOf=graphical-session.target`) for people who want it on login. No
     waybar module, no TUI, no tray at beta.
   * Logs: default `info` is one line per 5 s; `RUST_LOG=debug` for reports.
   *Verify:* fresh config, `od-sender list` shows all three receivers;
   `run --to iPhone` and `run --to <id>` both connect; the desktop entry
   launches in a terminal.
8. **CI: sway headless smoke + PKGBUILD build (§4.3 row 1).** One job in
   `linux.yml` running in an `archlinux:latest` container: `makepkg` of the
   sender PKGBUILD (first execution of it anywhere), then the Adopt smoke
   against `od-receiver --sink none`. *Verify:* green on a PR; the job fails
   when capture is deliberately broken (mutation check once).
9. **Release pipeline.** A `linux-release.yml` triggered by tags
   `linux-v*` (manual `git tag linux-v0.1.0-beta.1`): build on
   `ubuntu-24.04` and `ubuntu-24.04-arm`, run tests, upload
   `od-sender-<ver>-<arch>-unknown-linux-gnu.tar.gz` + `.sha256` to a GitHub
   **pre-release**. Until §3.5 option C lands, label the tarballs "built
   against FFmpeg 6 (Ubuntu 24.04); Arch users: install from the AUR". See
   §8.1 for the release-please question. *Verify:* dry-run on a fork or a
   `linux-v0.0.1-test` tag deleted afterwards; `tar` contents and checksums.
10. **AUR package.** Split the PKGBUILD: `opendisplay-sender` (depends
    `ffmpeg wayland`, makedepends `cargo pkgconf clang`, `arch=(x86_64
    aarch64)`, source = the `linux-v*` tag tarball, `.desktop` + user unit;
    `provides=(od-sender)`). The receiver stays out of the AUR until its own
    beta (its GStreamer dependency set would otherwise be pulled onto every
    sender user). *Verify:* `makepkg -si` here, `namcap`, `yay -S
    opendisplay-sender` from the published AUR package, `pacman -Ql`, run
    from `/usr/bin`.
11. **Website and docs.** In `src/App.tsx` add a third sender card "Linux
    <Beta>" next to macOS (reuse `.beta-tag` and the Android beta-steps
    pattern): `yay -S opendisplay-sender`, "Omarchy / Hyprland 0.55+; other
    compositors coming", link to `linux/README.md` and the release page;
    update the "Compatible apps" note and `index.html` meta that still say
    "Macs on the roadmap" / Apple-only. `README.md`: a "Linux sender (beta)"
    section; `linux/README.md`: install, run, config, `doctor`, the
    permission rule (§8.7), known issues, how to report (`doctor` output +
    `RUST_LOG=debug` log). Privacy page: no change (no telemetry).
    *Verify:* `pnpm build` locally; links resolve.
12. **Beta checklist run and merge.** Walk §1.2 on this machine, record
    results in PLAN.md M3 status, undraft PR #298 (or split the sender into
    its own PR if #298 is too large to review), merge, tag `linux-v0.1.0`.

Steps 5 and 6 can interleave with 7-11; 1-4 are prerequisites for everything.

---

## 6. After beta

In recommended order; each row says what it needs and how it is testable
with this one machine.

| Item | Needs | How to test with only this machine |
|---|---|---|
| **Adopt / mirror mode as a user feature** (`--output <name>`, §3.7) | Docs, the `panel`-bounds-the-stream rule for mirror, a `list-outputs` helper; the code exists from step 1 | Here, trivially (`--output eDP-1` mirror; `--output` on a `hyprctl output create headless` the user made); niri/COSMIC/river volunteers get support "for free" |
| **Cursor side channel** (`cursor`/`cursorImg`, UDP §6.3) | **Landed** (`wayland/cursor.rs`, `--cursor auto`/`video`/`off`): the video keeps its cursor until the first sprite arrives. **Off on Hyprland** (§8.14). On GNOME, Mutter's `cursor-mode` metadata would feed it; not wired yet | A compositor with working cursor sessions (Sway: unchecked) |
| **Static FFmpeg (§3.5 C)** | **Landed** for the release tarballs (`packaging/ffmpeg/build-static.sh`); the AUR package still links the system FFmpeg | First tagged release; run the tarball in an Ubuntu container |
| **Sway/wlroots backend** | **Landed** (`backend/wlroots`): create/unplug over Sway IPC, size/scale/position through `zwlr_output_manager_v1`, everything else `wayland/*` as-is. CI's headless Sway job runs `dev smoke --backend wlroots`, a full `run --backend wlroots` into `od-receiver --sink none` (~58 fps, e2e p50 ~17 ms at 1024x768 locally) and a `kill -9` + `clean` cycle. Left: a real (DRM) Sway session, other wlroots compositors (no output creation: adopt only) | Nested sway under Hyprland (§4.2) for a GPU-backed run |
| **KDE backend** | **Landed** (`backend/kde`): `zkde_screencast_unstable_v1` virtual output (v4 `with_description`, v2 fallback), frames from its PipeWire node (`src/pipewire.rs`, shared with the portal backend), input through `org_kde_kwin_fake_input` (no portal, no dialog). Scale and `--position` set through `kde_output_management_v2` like on Hyprland/Sway. Left: a real Plasma session (DRM backend), KWin < 6.8 with the shipped .desktop entry, refresh rates above KWin's fixed 60 Hz (`set_custom_modes`) | Plasma VM or a KDE user; CI `kde-smoke` (§4.3, green) |
| **GNOME backend** | **Landed** (`backend/gnome`) through Mutter's private ScreenCast/RemoteDesktop D-Bus APIs instead of the portal: no dialog, size via `RecordVirtual` `modes` (Mutter >= 50), scale and position via a temporary `DisplayConfig.ApplyMonitorsConfig` (§8.16), `--cursor off` → hidden. Left: a real GNOME session, Mutter < 50, cursor metadata | Headless Mutter here (`testbench/gnome-smoke.sh`); CI `gnome-smoke` |
| **Portal backend (Flatpak)** | ScreenCast `VIRTUAL` + RemoteDesktop portals, `PwSource.remote_fd` (untested); the GNOME chooser needs one click per receiver, then a restore token | A GNOME or KDE VM; not automatable headless |
| **VA-API (Intel/AMD, x86_64)** | `FrameBuf::Dmabuf` via `zwp_linux_dmabuf_v1` + gbm allocation, `Kind::Vaapi` with DRM hwframes (wl-screenrec is the reference); ahead of x264 in auto order; PLAN.md Spike 0 | Not testable here. Needs the Intel Omarchy machine or a volunteer; ship behind `--encoder vaapi` first |
| **x86_64 confirmation** | nothing new | A volunteer on Omarchy x86_64 running the §1.2 list; CI already builds it and the Arch-container job runs the smoke on x86_64 |
| **Flatpak** | Needs the portal backend (no compositor IPC or private D-Bus API in the sandbox) | Defer until the portal backend exists |
| **HEVC** | A hardware HEVC encoder (none on Asahi; x265 is too slow) | With VA-API hardware only |
| **Hyprland upstream** | Drafts in `linux/research/`: the cursor session always copies an empty sprite (unfixed on main); the `CScreenshareFrame::transform` abort is fixed on main (901881c9f, 212902547) but not in 0.56.2. Filing is the maintainer's call | — |

---

## 7. Protocol drift: what the sender must follow

`PROTOCOL.md` on `linux-plan` is identical to `main`. Checking the five
branches' diffs against `main` (`git diff main...origin/<branch> --
PROTOCOL.md`) shows their merge-bases are old: everything in
`feat-mac-mac` (§6.3 UDP cursor, §6.4 `addrs`), `fix/mirror-pixels-and-max-encode`
(`maxEncodeWide/High`), and `fix/receiver-capability-negotiation`
(`videoCaps`, `displayMaxFrameRate`, `streamConfig`) **is already in `main`**
(#145, #258, #288), as are `hello.panel` (#345), HEVC (#325) and `power`
(#332). Only `fix/ios-panel-follows-window` (= the first commit of
`feat/smooth-panel-transition`) is unmerged: a receiver in a window smaller
than its screen (iPad Split View, Stage Manager, foldables) reports the
window's pixels and re-sends `hello` on every resize/fold; the HEVC envelope
wording changes from "panel's long side" to "window's long side".

What that means for `od-sender`, in priority order:

| Spec item | Status in sender | Action |
|---|---|---|
| `hello.panel` (§6.7), sender decides the desktop | not parsed (`Hello` lacks `panel`) | step 3: parse, validate as §6.7 says, plan desktop from it; fall back to deprecated fields |
| `maxEncodeWide/High` + `videoCaps` intersection, High@L5.2 limits (§6.5) | parsed, ignored | step 3: `desktop.rs`; size output to the stream when capped |
| `streamConfig` before video and after each rebuild (§6.2) | done | keep; emit from the plan |
| `displayMaxFrameRate` (§6.1) | done (fps cap) | none |
| Re-sent `hello` on rotation/resize, 300 ms debounce (§6.1 + the unmerged window-follows change) | rebuild on every `hello` immediately | step 3: debounce; rebuild only when the plan changes |
| `pencil`/`proximity` (§6.1; receivers send them because we announce pv 3) | ignored, so a Pencil is dead on iPad | step 3: pointer mapping; pressure/tilt dropped |
| `cursorPort`, UDP side channel, `cursorAck` (§6.3) | session crate implements it; the host never sends `cursor` | after beta (cursor session) |
| `addrs`, cable upgrade (§6.4) | ignored | none: Linux has no host-to-host cable binding; discovery already prefers IPv4 |
| `power` (§6.6) | not parsed | parse and ignore; never offered (requires a direct-cable session we cannot have) |
| HEVC (§5, §6.5) | never selected | compliant; nothing to do until a hardware encoder exists |
| `welcome.min` = 1, `updateRequired` | done | none |

---

## 8. Risks and open decisions (with recommendations)

1. **Release stream.** `release.yml` uses release-please `simple`: a
   `feat(linux):` merge bumps the app version and triggers a Mac/iOS build
   with no Mac change. Options: (a) ride the app release (one version, Linux
   job added to `release.yml`; simplest, but every Linux fix ships a pointless
   Mac update); (b) separate `linux-v*` tags with a tag-triggered workflow
   and manual tagging; (c) release-please manifest mode with a `linux`
   component attributed by path (clean, but changes the Mac flow). *Recommend
   (b) for the beta, (c) once the beta flow is proven; merge Linux PRs as
   `feat(linux)`/`fix(linux)` either way and accept one spurious app bump at
   the #298 merge.*
2. **Packaging.** Source AUR (`opendisplay-sender`) at beta; static FFmpeg
   for generic tarballs after. *Recommend as in §3.5.* Open: package name
   (`opendisplay-sender` vs the existing all-in-one `opendisplay`); *recommend
   the split*.
3. **Output scale for phones.** `--scale passthrough` (default) gives the
   output the receiver's scale (3 on an iPhone); `--scale mac` uses the Mac
   sender's 2x rule; a number is fixed. Per desktop: Hyprland, Sway and KDE
   (`kde_output_management_v2`) apply it as given; GNOME snaps to the scales
   Mutter offers for the mode (an iPhone's 3 becomes 2). *Recommend `mac` as the default once
   readability on the phone is checked.* Left open for the user.
4. **Cursor baked into the video at beta.** Cursor moves at video latency
   and is absent on a static screen until damage. The side channel exists
   but cannot run on Hyprland (§8.14). *Accept for beta.*
5. **Mac receiver latency — answered.** The 220 ms was the sender's 4K
   software encode plus an oldest-frame handoff bug; after the fixes e2e
   p50 is 55-67 ms at 4096x2304. Remaining cost is software encode (#360).
6. **Hyprland IPC churn** (0.55 broke every `keyword` user). The backend is
   thin and verifies by reading `monitors` back, but a Lua API rename would
   break output creation outright. *Recommend the version gate + `doctor`,
   testing each Omarchy Hyprland bump on the nested harness before it reaches
   users, and `zwlr_output_manager_v1.set_custom_mode` for sizing once
   verified, which removes half the IPC surface.*
7. **Hyprland permission system — resolved.** On this machine
   `hyprctl getoption ecosystem:enforce_permissions` → `false`, not set
   (*verified*), and Omarchy 4.0.3's default config has no `hl.permission`
   rules. When a user turns it on: `screencopy` defaults to `ask`, which pops
   a dialog (Deny / Allow until the app exits / Allow until Hyprland exits);
   a denied client is **not refused, it receives black frames with a
   "permission denied" caption**, so the sender would stream a black
   display; rules are `hl.permission("<binary regex>", "screencopy",
   "allow")` (positional form, as in `/usr/share/hypr/hyprland.lua`) and are
   only read at startup (from the
   Hyprland wiki and the binary's `PERMISSION_TYPE_SCREENCOPY`/`KEYBOARD`
   strings; virtual-pointer input has no permission type, so it is not
   gated — *inferred* from those strings). Actions: `doctor` reads the
   option and warns; `linux/README.md` documents the rule for
   `/usr/bin/od-sender`; the nested harness keeps it off.
8. **x86_64 never run.** *Recommend shipping it as "expected to work" and
   asking for one tester before calling it tested; CI compiles it and the
   Arch-container smoke runs the capture/input path on x86_64 (sway, not
   Hyprland).*
9. **Receiver-side bugs found in step 5** (Android) may need app releases
   that are not in our hands here. *Recommend filing them and shipping the
   beta with "Android: works with app version >= X" wording.*
10. **Nested Hyprland — resolved, it works (§4.1).** Had it not come up
    (DRM backend grabbing the seat, allocator trouble through the parent),
    the fallback is an aarch64 VM with Omarchy as in PLAN.md §1, which is
    slower but equally safe. *Recommend trying nested first (step 2) before
    building anything else on it.*
11. **Third-party sender** (`tixwho/opendisplay-linux`). It covers KDE via
    portal+libkscreen and Hyprland via the xdph dialog, x86_64 only. Our KDE
    and GNOME paths now exist without dialogs. *Recommend keeping it listed
    until ours are verified on real desktops, and crediting its KDE
    findings (§4.4).*
12. **KDE path choice — resolved: zkde + fake input.** KWin 6.8 offers both
    globals to every unsandboxed client (sandboxed = systemd unit
    `app-flatpak-*`/`snap.*`; *verified* in `src/wayland_server.cpp` and live).
    KWin <= 6.7 offers them only to a program whose canonical
    `/proc/<pid>/exe` equals the first `Exec=` token of an installed .desktop
    entry listing them in `X-KDE-Wayland-Interfaces` (*verified* in 6.7.5
    source, not live); the package's `od-sender.desktop` now does that with
    `Exec=/usr/bin/od-sender run`. `fake_input.authenticate` always succeeds
    ("TODO: make secure"). No portal, no dialog, no RemoteDesktop.
13. **Hyprland < 0.57 crashes on ext-image-copy-capture when an output
    disappears under a frame request.** A race no client can close
    (`linux/research/hyprland-screenshare-transform.md`); its wlr-screencopy
    path checks the monitor at every step: on nested 0.56.2, full-rate 4K
    capture survived 40/40 output removals with wlr-screencopy, while ext
    crashed Hyprland at the 7th. `--capture auto` therefore uses
    wlr-screencopy on Hyprland < 0.57 (or of unknown version); `ext` can be
    forced. Revisit when Omarchy ships 0.57.
14. **Cursor side channel off on Hyprland.** Its cursor capture session
    always copies an empty sprite (unfixed upstream) and, on 0.56.2, a
    pending cursor frame crashes Hyprland when its output is removed. The
    Hyprland backend reports no cursor metadata, so the cursor stays in the
    video. Revisit per Hyprland release.
15. **Mutter's private D-Bus API.** `org.gnome.Mutter.ScreenCast`/
    `RemoteDesktop` are internal (gnome-remote-desktop uses them); `Version`
    has been 4 since Mutter 44, and `modes` arrived in 50 without a version
    bump. GNOME could restrict them to allow-listed callers as Shell did in
    41. *Mitigation:* `doctor` prints the API versions; the portal backend
    is the fallback if access is ever restricted.
16. **GNOME scale/position touches the real layout.** `ApplyMonitorsConfig`
    replaces the whole layout, temporarily: every other monitor is passed
    through from `GetCurrentState` (mode, scale, transform, primary, colour
    mode, RGB range, underscanning), nothing is saved, and Mutter restores
    the layout when the virtual monitor goes (*verified* headless). Not yet
    tried on a real multi-monitor GNOME desktop; mirrored monitors untested.
    The call is skipped when Mutter's own placement already matches.
17. **KDE `auto-left`/`auto-up` move the user's outputs.** KWin refuses
    negative positions, so the new output goes to 0,0 and the other outputs
    shift right/down in the same configuration, as KDE's display settings
    do; the sender moves them back when it closes. **Resolved:** after a
    `kill -9`, KWin 6.8 itself moves them back within 0.3 s (*verified* on
    the headless bench: it restores the layout when the client dies); a
    starting sender also re-normalises any offset (`normalize` now shifts
    back from positive offsets too), and `od-sender clean` on KDE moves the
    layout to 0,0 when no sender holds a lock. That last path is a safety
    net for other KWin versions; it could not be staged live, since KWin and
    kscreen-doctor normalise a lone offset output to 0,0 themselves.
