# OpenDisplay Linux sender: engineering review before public beta

Branch `linux-plan` (PR #298), reviewed 2026-10-20, read-only. `cargo clippy
--workspace --all-targets --all-features` is clean; `cargo test` passes for
od-sender (124), opendisplay-proto, opendisplay-session and od-fake-sender.
`od-receiver` `tests/e2e.rs::decodes_with_fakesink_when_a_decoder_is_installed`
fails on this host (T1). Nothing was run against a compositor. Protocol
conformance and packaging/CI were delegated to two sub-reviews; every item
below that came from them and is in the top 10 was re-checked by me at the
cited lines.

Paths are relative to `linux/crates/` unless they start with `.github` or
`linux/`. Severity: **blocker** (fix before the first tag), **should-fix**
(before beta if cheap, else first patch release), **nit**. "suspected" =
reasoned from the code, not reproduced.

---

## 1. Correctness and safety

### S1. `hello.id` reaches Hyprland's Lua `eval` and IPC lines unescaped — should-fix

`od-sender/src/session.rs:116-121` builds the output name as `od-` + the first
six characters of the receiver-supplied `hello.id`, lower-cased, with no
character check. That name is interpolated into:

* `backend/hyprland/ipc.rs:190-193`: `hl.monitor({output="{name}", …})` sent
  through `eval`. `workspace::lua_str` (`backend/hyprland/workspace.rs:128`)
  exists for exactly this and is not used here.
* `ipc.rs:167` `output create headless {name}` and `ipc.rs:251`
  `output remove {name}` (space-separated IPC: a space in the id splits the
  command).
* the lock path `$XDG_RUNTIME_DIR/opendisplay/{name}.lock`
  (`backend/lock.rs:40-42`) and the wlroots record file.

Six characters is too short for useful code execution (`"})--` closes the
call cleanly), so the practical impact is a receiver that can make the sender
fail or create an output with an odd name, plus lock-name collisions for ids
sharing a prefix. The receiver is any TCP peer the user dials. Fix: derive
the suffix from a hash of the id (first 6 hex of FNV-1a/SHA-256) or reject
anything outside `[a-z0-9]`, and route the Hyprland name through `lua_str`.
Add a test in `session::tests`.

### S2. Virtual-pointer button can stay pressed after the sender exits — should-fix (exit criterion 2 names it)

`od-sender/src/wayland/input.rs:163-167`: `InputInjector::drop` only sends
`InputCmd::Stop`; the release of a held `BTN_LEFT`, `pointer.destroy()` and
the final roundtrip run on the injector thread afterwards and nothing joins
it. `session::Active::close` (`session.rs:411-424`) drops the injector, then
removes the output; `main` returns and calls `process::exit` (`main.rs:223`).
On a Ctrl-C mid-drag the process can exit before the release is flushed;
wlroots keeps the seat's button count and Hyprland does not synthesise
releases for a destroyed virtual pointer. GNOME's `RemoteInput`
(`backend/gnome/input.rs:314-346`) has the same shape (Mutter releases with
the session, so less exposed); KDE's `FakeInput` releases synchronously in
`Drop` (`backend/kde/input.rs:700-708`) and is fine. Fix: keep the
`JoinHandle`, join in `Drop` with a ≤ 500 ms bound; release the button when
the TCP session ends, not only on `Ended/Cancelled`.

### S3. `outman::apply` can wedge the session thread forever — should-fix

`backend/wlroots/outman.rs:369-371` and `:394-396` loop on
`queue.blocking_dispatch` with no deadline and no stop flag, on the tokio
session thread (`build` → `reconfigure`). If Sway never answers (another
client's config in flight, compositor stalled) the sender hangs with the
output created and capture stopped, and SIGINT is not observed because the
executor is blocked. `backend/kde/outputs.rs:98-120` does this right with
`dispatch_with_timeout` and a 5 s deadline; copy it. Same pattern, lower
likelihood: `wayland/capture.rs:429-434` (`while !st.session.done {
blocking_dispatch }`) ignores `stop`, so `CaptureThread::stop` can block its
joiner during session setup; `wayland/cursor.rs:765-792` roundtrips before
`ready` while `CursorThread::spawn` waits on `ready_rx.recv()` unbounded.

### S4. GNOME: a failed `VirtualMonitor::create` leaks the Mutter session and a thread per attempt — should-fix (suspected)

`backend/gnome/mutter.rs:229-260`: the `mutter-signal` thread iterates a
`MessageIterator` with no timeout, holding a clone of the bus connection. On
the 5 s `PipeWireStreamAdded` timeout the function bails before `Self`
exists, so `RemoteDesktop.Session.Stop` is never called; Mutter closes a
session only when the owning connection goes away, which the leaked thread
prevents. With `reconnect` on, every redial can leave one more virtual
monitor on the desktop until the process exits. Fix: construct the
`VirtualMonitor` (so `Drop` stops it) before waiting for the signal; bound
the wait (async proxy + timeout, or a `receive_signal` stream dropped on
exit); stop the session on every `?` after `CreateSession`.

### S5. `PwCapture::spawn` timeout leaves the PipeWire loop running with the sink — nit

`od-sender/src/pipewire.rs:460-462`: on the 5 s `ready` timeout the function
returns `Err` but the thread keeps running `mainloop.run()` (quit is only
sent by `stop()`), still feeding a sink the session will rebuild. Unlikely
(`ready` is sent right before `run`); on that path send quit and join.

### S6. KDE recreates the output on `--cursor off` and on every rebuild — nit (flicker, double layout shift)

`backend/kde/mod.rs:247-259`: `KdeDisplay` starts with `Pointer::Embedded`;
`start_capture(cursor: false)` destroys and recreates the KWin output and
re-runs `outputs::configure`, which can shift the user's outputs twice for
`auto-left`. `Backend::set_cursor_visible` is already called before the first
`open` (`session.rs:670`) and GNOME uses it; do the same in `KdeBackend`.

### S7. Compositor round-trips run inline on the async session loop — should-fix (robustness)

`session.rs:954-978`: `display.drifted()` runs a synchronous Hyprland/Sway
IPC request (connect + up to 5 s read timeout, `hyprland/ipc.rs:126-134`)
every 2 s on the tokio thread; `build()` (`session.rs:430-501`) runs the whole
reconfigure inline: Hyprland `configure` polls up to 1 s, KDE `make`/`settled`
up to 5 s, GNOME does D-Bus calls, `show_workspace` sleeps 100 ms. Meanwhile
no receiver bytes are read, no frames go out and `shutdown` is not observed.
Backpressure holds (bounded `encoded_tx`, latest-wins slots), so it is not a
leak, but a slow compositor becomes a stalled stream and a slow Ctrl-C. Move
`build`/`drifted` to `spawn_blocking`, or at least bound every poll.

### S8. Threads not joined / not bounded — nit

* `EncodePipeline::drop` (`session.rs:353-357`) only sets `stop`; the
  `JoinHandle` from `spawn_reporting_panics` is discarded, so a rebuild starts
  a second x264 at `threads = all cores` while the first winds down.
* `ReloadWatcher::drop` (`backend/hyprland/events.rs:72-78`) can wait up to
  1 s in the reconnect `sleep` without checking `stop`.
* `wayland/capture.rs:477-481`: with all `MAX_BUFFERS` leased the loop
  `continue`s without dispatching, so a `Stopped` event is not seen until a
  lease returns. Only matters with a wedged encoder.

### S9. Lock directory fallback is world-shared — nit

`backend/lock.rs:53-58`: without `XDG_RUNTIME_DIR` the locks go to
`/tmp/opendisplay` with umask permissions; another user can hold
`od-<id>.lock` and the sender refuses to start. Refuse to run without
`XDG_RUNTIME_DIR`, or create `0700` and fail if it is not ours.

### S10. Adopt mode on Hyprland < 0.57 uses the crash-prone capture path — nit

`backend/adopt.rs:431-443` has no `capture_preference`, so `--output <name>`
on Hyprland 0.56 picks ext-image-copy-capture (`detect.rs::choose_capture`),
the path §8.13 says crashes the compositor when the output goes away under a
frame request. `decide` already knows `hyprland_socket`; pass the preference
to `AdoptBackend`.

### Checked and fine

* `CapturedDisplay` ordering (`wayland/display.rs:70-80, 181-192`): capture
  and cursor threads joined before `remove()` on close, drop and panic
  unwinding; `released` makes it idempotent; unit-tested.
* shm leases (`wayland/shm_pool.rs`): `Mapping` refcounted by every `Lease`,
  generations keep stale leases out of the free list, `MAP_FAILED` checked,
  `munmap` once. Dropping the `File` after `create_pool` is fine (the mapping
  pins the file; wayland-backend dups request fds).
* Convert stage and encoder hand-offs are non-blocking latest-wins slots
  (`latest.rs`, `encoder/stage.rs:125-134`), so `CaptureThread::stop` cannot
  deadlock on the sink. `av_frame_make_writable` before each banded convert
  keeps pooled `YuvFrame`s safe while the encoder still references them;
  `convert.rs` bands write disjoint rows, bounds checked at `:119`.
* Signals: SIGINT/SIGTERM/SIGHUP end the session through the normal close
  path; SIGPIPE is ignored by the Rust runtime; a panic in capture, encode or
  the session thread reaches `CapturedDisplay::drop`.
* `LossBudget`/drift budget (3 per 60 s) stop rebuild storms.
* `capture_preference` defaults to wlr-screencopy on unknown Hyprland
  versions; the wlr path ends on `GlobalRemove` of the target.
* Lock: `flock` + `LOCK_NB`, pid written after the lock, held by `clean`
  while removing; `HyprlandOutput::open` removes an output it created when
  sizing fails.

---

## 2. Protocol conformance (PROTOCOL.md pv 3)

Framing, demux, unknown-type handling, hello validation and precedence,
debounce, streamConfig/videoCaps/L5.2 limits (bit-identical to
`StreamConfiguration.swift`), IDR gating, SPS/PPS change handling, clock
sync, kf rate limiting, cursor datagram/ack sequencing and touch/scroll units
all match the spec. Two interop defects, both in hosts rather than the proto
crate:

### P1. Sender restarts the cursor sequence on every `hello` — should-fix (verified)

`opendisplay-session/src/sender.rs:300` sets `self.cursor_seq = 0` inside
`ControlMessage::Hello` handling. §6.3: the sequence is per TCP session and
restarts only on (re)connect; the Mac sender resets only in `becomeReady`
(`Mac/MacSender.swift:1534`) and keeps it across a rotation re-hello (`:2121,
2138`); Mac/iOS receivers (`Shared/StreamReceiver.swift:592, 916-918`) and
`ReceiverSession` (`receiver.rs:295, 369-376`) enforce `s > last` and reset
only on a new TCP connection or UDP flow. After any re-sent `hello` (iPad
rotation, Mac receiver `addrs` change) every `cursor` with `s` = 1, 2, … is
dropped until it passes the old high-water mark: the receiver's cursor
freezes. The reset in `SenderEvent::Connected` (`sender.rs:170`) is the right
one; delete line 300 and add a loopback test (connect, cursor, `update_hello`,
cursor, assert applied).

### P2. `od-receiver` sends no `hello.panel` — should-fix (verified)

`od-receiver/src/main.rs:412-420` fills only the deprecated trio plus
`scale`. §6.1/§6.7: `pixelsWide/High` are "pixels of a 2x desktop" and the
Mac sender never reads `hello.scale`; it treats a missing `panel` as scale 2
(`Mac/PhoneInfo.swift:32-41`), so a Linux receiver on a 1920x1080 scale-1
monitor gets a 960x540-point HiDPI desktop from a Mac sender. Linux→Linux is
unaffected only because `desktop.rs` `Passthrough` reads `hello.scale`.
Populate `panel` (pixels + scale, points absent) and optionally
`videoCaps: [{codec: "h264", maxFrameRate: refresh}]`.

### Nits

* `desktop.rs:219-224, 382-384`: target fps = `displayMaxFrameRate` clamped
  30..240; the Mac uses `min(60, displayMaxFrameRate)`
  (`StreamConfiguration.swift:113-115`). A legacy receiver on a 120 Hz panel
  gets 120 fps announced and encoded in software. Consider `.min(60)` unless
  an h264 entry says more.
* `session.rs:1018-1023`: a `PlanError` closes the TCP connection before
  `welcome` is written; the receiver sees only a drop. Spec has no message
  for it; acceptable.
* Wire order on the first hello: `streamConfig` is written by `apply()`
  (`session.rs:1033 → 815`) before the `welcome` action of the same batch
  (`:993-995`); Mac sends `welcome` first. No receiver found that cares.
* `control.rs:65-69`: `pixelsWide/High/scale` are required fields; a future
  panel-only `hello` is `Malformed`. `#[serde(default)]` now makes the next
  bump a no-op.
* `SenderSession::set_health` has no caller in od-sender; sender pings carry
  no health fields (allowed).
* `video.rs:378-410` `normalize_start_codes`: a `00 00 00 00 01` mid-stream
  leaves a stray `00` on the previous NAL; not produced by any encoder here.
* Scroll under `ScalePolicy::Mac` with explicit points (6400x3600 desktop,
  4096x2304 stream) is off by the stream/output ratio; same simplification as
  the Mac.
* `pencil`/`proximity` parse but are dropped (`backend/mod.rs:396-397`,
  `session.rs:1060`); already SENDER-PLAN §5 step 3.

---

## 3. Architecture

### A1. Two display implementations enforce the crash-ordering rule by hand — should-fix

`CapturedDisplay` (`wayland/display.rs`) owns "stop capture, then touch the
output" for Hyprland/Sway/adopt, with tests. KDE (`backend/kde/mod.rs:301-318`)
and GNOME (`backend/gnome/mod.rs:208-226`) re-implement it in `release`/`Drop`
with field-order comments and get no `set_cursor_painted`, `drifted` or
`start_cursor`. A `PwDisplay<O: OutputControl>` (or `CapturedDisplay` generic
over a `FrameSource`) gives both backends the tested ordering.

### A2. The capture-protocol choice leaks into the generic `Backend` trait — nit

`backend/mod.rs:197-236` has three methods for one Wayland-only concept
(`wayland_capture`, `capture_preference`, `set_capture`); PipeWire backends
stub two and `detect.rs` warns when `--capture` is set. The Wayland backends
could take a `CapturePath` in their constructors and the trait lose all three.

### A3. Placement is implemented three times, inconsistently — should-fix

`backend/mod.rs:99-127 place()` (Sway, KDE), KDE's `normalize`
(`kde/outputs.rs:233-237`), and GNOME's own arithmetic
(`gnome/layout.rs:529-547`). GNOME `AutoRight` lands at `(right, 0)`, `place()`
at `(right, edge.y)`; GNOME `AutoLeft` is `(-ow, 0)`, `place()` is
`(edge.x - w, edge.y)`. Hyprland delegates to its own `auto-*` rules. Route
GNOME through `place()` plus KDE's normalise step so `--position` means the
same thing on every desktop.

### A4. Dead surface in the CLI — nit

`BackendId::Portal` is a `ValueEnum` variant (`--backend portal` is offered
by clap and always refused, `detect.rs:251-256`); `Caps.dmabuf` and
`Frame.presentation_ns` are never read; `FrameBuf::Dmabuf` is unreachable.
`#[value(skip)]` on `Portal` until it exists.

### A5. KDE/GNOME have no `drifted` and nothing for `clean` — nit (documented)

Fine while both compositors drop the output with the client; say so in
`linux/README.md` next to `od-sender clean`.

---

## 4. Packaging, release, CI

No blockers. Supply chain is sound: `build-static.sh:21-41` pins x264 and
FFmpeg by full commit and fails on a mismatch; no `curl | sh`; the cache key
hashes the script; PR runs cannot write the release cache; `contents: write`
is scoped to the release job; artifacts are built, unit-tested, smoke-tested
and checksum-verified in the same job from the tag checkout; aarch64 runs
natively. The `.desktop` entry matches KWin 6.7.5's trust check
(`serviceutils.h`: first `Exec=` token, canonical path, `X-KDE-Wayland-Interfaces`).

### C1. glibc/libstdc++ floor is printed, not enforced — should-fix (verified)

`linux-release.yml:106-116` only echoes the newest `GLIBC_` symbol; the
release notes (`:188`) promise "glibc 2.38 or newer (Fedora 39)" but the host
is Ubuntu 24.04 = glibc 2.39, and OpenH264 (C++, GCC 13.2) pulls
`GLIBCXX_3.4.32` (suspected, unchecked). Fail the step when the newest
symbol exceeds the promised floor, or build in an older base
(Debian 12 / manylinux_2_28), or link `-static-libstdc++`.

### C2. Tarball ships no third-party licence texts — should-fix

`linux-release.yml:135-137` packs only the project LICENSE. Static x264
(GPL-2+) + FFmpeg `--enable-gpl` + GPL-3.0-only workspace is a consistent
GPL-3 binary, but the artifact carries no FFmpeg/x264 COPYING, no OpenH264
BSD notice (built from source via the `source` feature, so Cisco's patent
grant does not apply; say so), no MIT/Apache attributions for the Rust
crates. Add `THIRD-PARTY-LICENSES` (`cargo about` + the three C licences);
have `build-static.sh` copy licence files into `$prefix/share/licenses` so
they ride the cache.

### C3. Release binary is not stripped — nit (verified)

`linux/Cargo.toml:54-56` has no `strip`; no strip step in the workflow;
SENDER-PLAN §3.5 says "stripped binary 7.3 MB". Add `strip = "symbols"` and
`tar --owner=0 --group=0 --mtime` for reproducible tarballs.

### C4. CI gaps — should-fix (verified)

* `linux.yml:68-85`: fmt/clippy/test/build run without `--locked`; a stale
  `Cargo.lock` passes here and fails only in `arch-package` (`--frozen`) or at
  tag time.
* `linux.yml:14-19` + `linux-release.yml:17-24`: a `linux-v*` tag on a
  non-main commit runs only the release workflow (unit tests + encode-bench):
  no clippy/fmt, no proto/session tests, no sway/kde/gnome smokes. Add
  `on.push.tags` to `linux.yml` or require `git merge-base --is-ancestor
  origin/main`.
* `linux-release.yml:20-23`: PR path filter covers only
  `linux/packaging/ffmpeg/**`; a crate or `Cargo.lock` change that breaks the
  static link is first seen at tag time. Add `linux/Cargo.*` and
  `linux/crates/od-sender/**`.
* `linux.yml:163-188` (`kde-smoke`): the comment and SENDER-PLAN §4.3 say a
  vgem-less runner fails the job; the step warns and skips, so a required job
  can pass with zero coverage. Pick fail, or `continue-on-error` + not
  required, and make the doc agree.
* All actions pinned by tag, not SHA; at least SHA-pin the release job
  (`contents: write`) and `release.yml`.
* `nasm` is a PKGBUILD makedepend but `linux.yml:50-58` never installs it, so
  x86_64 CI tests OpenH264's C fallback, not what the AUR builds.
* No `timeout-minutes` on the container jobs (6 h default; no cargo/pacman
  caching).

### C5. Packaging nits

* `od-sender.desktop:5`, `od-sender@.service:12` hard-code
  `/usr/bin/od-sender` and ship in the tarball whose notes say "put od-sender
  on your PATH": on KWin ≤ 6.7 the trust check then fails silently. Document
  `/usr/local/bin` + `sed`, or ship a tarball-specific pair.
* `PKGBUILD:20-25`: `optdepends` lacks `pipewire`/`wireplumber` (the KDE and
  GNOME backends need the daemon).
* `PKGBUILD:21`: soname deps (`libavcodec.so` …) let rebuild-detector flag
  the FFmpeg bump the header comment relies on.
* `linux-release.yml:95-101` + `build-static.sh:80-92`: static archives are
  linked via global `RUSTFLAGS` (hits build scripts and proc-macros too);
  use `CARGO_TARGET_<TRIPLE>_RUSTFLAGS`.
* `od-sender --version` prints `0.1.0`, not the tag (`CARGO_PKG_VERSION`).
* `rust-toolchain.toml` `stable` is unpinned.
* `tests.yml:205-207` has no path filter: every Linux-only PR runs the macOS
  Xcode build.
* `namcap` (`linux.yml:119`) never fails the job.

### C6. SENDER-PLAN.md drift — nit

§3.5 "fails on any NEEDED beyond glibc/libgcc/libstdc++" (allow-list also has
`libpipewire-0.3.so.0`); "stripped binary" (nothing strips); the option-C row
describes `ffmpeg-sys-next` build features but the implementation is
`FFMPEG_DIR` + a hand-rolled link group; §8.2 says static tarballs come
"after" beta while §3.5 status and the release notes ship them at beta; §4.3
KWin row "green and required" contradicts the skip logic; "gated on paths
for their backend directory" is actually `linux/**`.

---

## 5. Test gaps

### T1. `od-receiver` e2e decode test fails where a non-working decoder is installed — should-fix (verified here)

`od-receiver/src/video.rs:263-274` `has_h264_decoder()` accepts any
MARGINAL-ranked element with an `x-h264` sink pad; on this host that is
`v4l2slh264dec` (no such hardware; `avdec_h264`/`vah264dec` absent), so
`open()` succeeds, decodebin3 never produces a frame, and `tests/e2e.rs:141`
fails with `rendered 0 of 120` instead of skipping. The same logic misleads
users: the receiver starts, later prints "frames pushed, none reached the
sink" and never says "install gst-libav". Prefer/require
`avdec_h264`/`vah264dec`/`openh264dec`, or probe once by pushing the test
clip through `decodebin3 ! fakesink`; skip the test when the probe fails.

### T2. Nothing drives `session()` end-to-end with a stub backend

`session.rs` tests cover `target`, `LossBudget`, `Backoff`, lock conflicts,
cursor state and config layering, but no test runs `session()` against a
`Backend` stub over a loopback `ReceiverSession`: hello → build →
streamConfig → IDR → `Lost` → rebuild → `closing`. S2, S3 and S7 would show
up in such a test with a stub that blocks or panics.
`opendisplay-session/tests/loopback.rs` is the harness to extend.

### T3. Protocol-level gaps (from the sub-review)

`loopback.rs`: no cursor-sequence continuity across `update_hello` (P1); no
`touch`/`scroll`/`pencil`/`stats`/`sleeping`/`closing` dispatched through
`SenderSession`; no `cursorImg` through both sessions; no `updateRequired`
reaching `ReceiverAction`; no `cursorPort` absent→present→absent toggle.
`od-fake-sender/tests/e2e.rs`: `drop_every` (kf recovery) and `stats`
reception never exercised; no assertion that the first frame after reconnect
carries SPS+PPS; no UDP cursor path. `od-receiver/tests/e2e.rs`: UDP
datagram → `cursorAck` → TCP mirror stops untested; `closing` on shutdown not
asserted; no test on `main.rs`'s hello construction (P2).

### T4. Output-name derivation untested for hostile ids (S1)

### T5. Live-backend safety paths run only on Sway

`linux/testbench/*.sh`: `kill -9` + `clean` on Sway only; KDE/GNOME smokes do
one open/capture/close. The lock-conflict path (`pre_dial_conflict`,
`LockHeld` from `open`), reload restore and output-lost recreation are
unit-tested or nested-Hyprland-only, never in CI.

### T6. Hyprland IPC parsing has one fixture

Add `j/version` for 0.55 and a `-dirty` build, `j/monitors` with a disabled
monitor and `scale: 1.333333`, and a `configure()` read-back that snaps a
fractional scale (`scale_snapped`).

### T7. desktop.rs

No case for `displayMaxFrameRate` < 30 or a legacy receiver other than 120 Hz
above the Mac's 60 cap; no test that `stream_for` with an adopted size
announces what the SPS will carry.

---

## Top 10 for the beta

1. **P1** Delete the `cursor_seq = 0` reset in `Hello` handling
   (`opendisplay-session/src/sender.rs:300`); add the loopback test. Every
   re-sent `hello` otherwise freezes the receiver's cursor.
2. **S1** Sanitise/hash `hello.id` before it becomes the output and lock name
   (`session.rs:116`), and pass the Hyprland name through `lua_str`
   (`hyprland/ipc.rs:190`).
3. **S2** Join the input thread in `InputInjector::drop` (bounded) so the
   button release reaches the compositor before exit; release on session end.
4. **S3** Bound the `blocking_dispatch` loops in `wlroots/outman.rs:369-396`
   (and `wayland/capture.rs:429`, `cursor.rs` setup) with the KDE
   `dispatch_with_timeout` pattern.
5. **P2** Send `hello.panel` from `od-receiver` (`main.rs:412`) so a Mac
   sender does not treat a 1x Linux panel as 2x.
6. **C1 + C2** Enforce the glibc/GLIBCXX floor (or build on an older base)
   and ship third-party licence texts in the release tarball
   (`linux-release.yml:106-137`).
7. **C4** `--locked` in `linux.yml`; run `linux.yml` on `linux-v*` tags (or
   require the tag on main); widen the release PR path filter; decide whether
   `kde-smoke` may pass vacuously and align the doc; SHA-pin the release
   job's actions.
8. **S4** Stop the Mutter session on every error path in
   `VirtualMonitor::create` and bound the signal wait (`mutter.rs:229-260`).
9. **T1** Make `has_h264_decoder()` require a decoder that decodes (or
   probe), so the receiver fails loudly and the e2e test skips instead of
   failing on hosts with only `v4l2slh264dec`.
10. **T2 + S7** One `session()` test against a stub backend over loopback
    (hello → build → IDR → Lost → rebuild → closing), then move `build`/
    `drifted` off the async thread; **S6** and **A3** (KDE double create,
    GNOME placement) ride along as small follow-ups.
