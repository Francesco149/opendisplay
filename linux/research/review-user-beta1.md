# OpenDisplay Linux beta: user and release-lead review

Reviewed 2026-10-20, read-only. Sources: `origin/linux/site-beta` (linux/README.md,
README.md, src/App.tsx, index.html, public/privacy.html) and `linux-plan`
(linux/SENDER-PLAN.md §1.1/§2/§8, linux/research/release-notes-beta1.md,
linux/packaging/arch/opendisplay-sender/*, .github/workflows/linux-release.yml,
linux/crates/od-sender/src/{main,config,doctor,discover,session}.rs,
backend/{detect,lock}.rs). Nothing was run.

---

## Part 1: six personas, first ten minutes

### 1. Omarchy user, Intel laptop, iPad, new to Linux internals

Path: website Linux card -> "Get started" -> linux/README.md -> `yay -S opendisplay-sender`.

- **Confused at minute 0 (website vs README disagree).** The site card says
  "Coming soon to the AUR as `yay -S opendisplay-sender`... Today: build from
  source", the FAQ says "An AUR package ... and prebuilt tarballs are coming;
  until then it builds from source", but the README it links to leads with
  `yay -S opendisplay-sender` as if live. Whichever is true on launch day, the
  other one is wrong. Suggested: flip all three in the same commit as the
  tag, and until then make the README's Install section say "(available once
  `linux-v0.1.0-beta.1` is tagged)".
- **Their machine is not in the "Tested" row.** Tested = aarch64 (Asahi). An
  Intel laptop is "Expected to work". The site card says "Tested on Omarchy /
  Hyprland 0.55+" with no architecture caveat, so they read "tested" and will
  file a bug as a regression if anything differs. Suggested card text: "Tested
  on Omarchy / Hyprland 0.56 (Apple silicon via Asahi Linux); x86_64 is expected
  to work and needs testers."
- **AUR build cost is unstated.** `makedepends` pulls `clang` (bindgen) and
  `nasm`; `check()` runs `cargo test --release`. A novice watching `yay` compile
  for many minutes with no number in the docs assumes it hung. Suggested:
  "Builds from source: expect 5-15 minutes and ~2 GB of build dependencies
  (rust, clang). Prebuilt tarballs are the quick path." (Measure the real time
  on an x86_64 laptop and put the number in.)
- **"First receiver found" is a trap with two devices.** They own an iPad and
  possibly an iPhone with the app open; `od-sender run` silently picks whichever
  answers first. README says "discover receivers and connect to the first one
  found" as if that were a feature. Suggested: when more than one receiver is
  discovered and `--to` is not set, print the list and exit with "several
  receivers found; pick one with `--to <name>`" (or at least log "connecting to
  X; use --to to choose" at INFO before dialling). Document it the same way.
- **Where did the display go / how do I put a window on it?** The README says
  "The new output appears to the right of your current monitors" and that a
  workspace is shown there, but an Omarchy newcomer does not know how to move a
  window to another monitor. Suggested one line in Usage: "On Omarchy, move a
  window there with Super+Shift+Right (or drag it); `--workspace 8` shows
  workspace 8 on the iPad instead." Also name the output as Hyprland shows it
  (`od-...`), so `hyprctl monitors` makes sense to them.
- **Stopping.** Ctrl+C is documented; the menu entry opens a terminal and
  `Terminal=true` means closing it sends SIGHUP, which is handled. Good. But
  the desktop entry runs `od-sender run` with no `--to`, so the menu entry and
  the "first receiver found" trap combine. Suggested: the desktop entry's
  Comment should say "Connects to the first receiver found; set `to = ...` in
  ~/.config/opendisplay/sender.toml to choose", or ship a second `Exec` that
  uses `od-sender list` first.
- **Uninstall is undocumented.** `yay -R opendisplay-sender` leaves
  `~/.config/opendisplay/sender.toml`, the enabled user unit symlink and, after a
  crash, an `od-*` output. Suggested README section "Uninstall": disable the
  unit, `od-sender clean`, remove the package, optionally delete the config dir.
- **What they report as success:** iPad shows the desktop within a minute of
  `yay` finishing and taps click. **Give up:** `yay` takes 20 minutes, or
  `run` prints `no receiver found within 10s` with no hint (see persona 2).

### 2. Ubuntu 26.04 GNOME user with an Android phone

Path: website -> FAQ "Does it work on Linux?" -> README -> tarball.

- **The tier word scares them off before they try.** "GNOME 50+ ... Verified on
  headless compositors only" and "none has run on a real desktop session yet"
  is honest, and good. But the same README then describes GNOME behaviour in
  confident detail (temporary display configuration, layout restore). Suggested:
  keep the detail, add "(verified on a headless Mutter in CI; please report
  whether it holds on your desktop)" at the top of the GNOME subsection.
- **"Unpack and put `od-sender` on your PATH" is not a step for this person.**
  Suggested concrete commands: `tar xf od-sender-*.tar.gz && install -Dm755
  od-sender-*/od-sender ~/.local/bin/od-sender` plus "log out and in if
  `~/.local/bin` was just created".
- **The tarball's desktop entry and unit are unusable as shipped.** Both
  hardcode `/usr/bin/od-sender` (`od-sender.desktop` `Exec=`, `od-sender@.service`
  `ExecStart=`). A tarball user who copies them to `~/.local/share/applications`
  and `~/.config/systemd/user` gets a dead launcher and a failing unit. The
  README says "Each tarball also carries the desktop entry and the systemd
  --user unit" as if they were ready. Suggested: either install to
  `/usr/local/bin` in the tarball instructions and ship the files with that
  path, or document `sed -i 's#/usr/bin/od-sender#'"$HOME"'/.local/bin/od-sender#'`
  for both files, and `systemctl --user daemon-reload`.
- **`Terminal=true` on GNOME.** GNOME has no reliable terminal launcher for
  `Terminal=true` entries on every setup; the menu entry may do nothing
  visible. Verify on a stock Ubuntu session before claiming "shows up in the
  menu" for non-Omarchy users.
- **Android receiver is "Expected to work", one session logged.** The release
  notes draft includes Pixel 10a numbers (53 fps, 33 ms) but still lists
  Android as expected-to-work because the functional checklist is pending.
  That is fine, but say what is pending: "touch, scroll, rotation, lock and quit
  not yet checked on Android".
- **Scale surprise.** README: "GNOME rounds to the nearest scale it offers
  (an iPhone's 3 becomes 2)". For a 2424x1080 phone the result is unclear from
  the docs; they will see either tiny text or a cropped desktop and not know
  which knob to turn. Suggested: "If text on the phone is too small, try
  `--scale 3`; GNOME may pick the nearest value it supports."
- **Discovery failure has no hint.** `run` ends with `no receiver found within
  10s` (discover.rs:94); `list` prints `no receivers found` and exits 1. The
  Mac README has a whole "Permissions checklist" for this; the Linux README has
  nothing. Suggested error text: "no receiver found within 10 s. Is the
  OpenDisplay app open and in the foreground on the device, on the same WiFi
  (not a guest network)? On iPhone/iPad, Settings > Privacy & Security > Local
  Network must allow OpenDisplay. Try `od-sender list --timeout 10`, or
  `od-sender run --to <ip>:9000`." Mirror it in a README "Troubleshooting"
  section.
- **Success:** phone shows the desktop, Settings > Displays lists a new monitor.
  **Give up:** `no receiver found`, or the virtual monitor appears but the phone
  stays black with no message about why.

### 3. KDE Plasma user on Fedora with a spare Mac

- **Which Plasma do they have?** Fedora in October 2026 mostly runs Plasma
  6.7; 6.8 is days old. So the common case is the "6.7 and older: only with the
  packaged `/usr/bin/od-sender`" clause. For a tarball user that means the
  binary must be at exactly `/usr/bin/od-sender` (KWin compares
  `/proc/<pid>/exe` to the desktop entry's `Exec`) and the `.desktop` file must
  be installed where KWin sees it. The README says "(the AUR package and the
  tarballs ship `od-sender.desktop`)" and stops. Suggested exact steps:
  `sudo install -Dm755 od-sender /usr/bin/od-sender`,
  `sudo install -Dm644 od-sender.desktop /usr/share/applications/`, then
  `kbuildsycoca6` or re-login, then `od-sender doctor` should show both
  globals. Also state plainly: "a binary in `~/.local/bin` cannot work on
  Plasma 6.7 and older".
- **Fedora's `ffmpeg-free` note is in the wrong row.** The "Expected to work"
  row mentions Fedora's OpenH264 fallback inside the *Hyprland* sentence; a
  Fedora KDE reader skims past it, then wonders why the source build is slow.
  It only matters for source builds anyway (the tarball is static). Suggested:
  move it to "From source" as "On Fedora, `ffmpeg-free` has no libx264; use the
  RPM Fusion `ffmpeg` or the prebuilt tarball, otherwise the slower OpenH264
  encoder is used."
- **The KDE crash text contradicts the release notes.** README: "they move
  back when the sender exits, but not after a crash: then rearrange them in
  System Settings". Release notes: "they move back when it goes, also after a
  crash (KWin 6.8 does that itself; `od-sender clean` is the fallback)".
  SENDER-PLAN §8.17 says the latter was verified on 6.8. Pick one; suggested
  README text: "KWin 6.8 puts them back by itself if the sender crashes; on
  older Plasma run `od-sender clean` or rearrange them in System Settings >
  Display."
- **The Mac receiver side.** They need "OpenDisplay Receiver" from the
  releases page; the Linux README mentions "releases page" once in the intro
  without a link. Suggested: in Usage, "Receivers: iPhone/iPad (App Store),
  Android (Google Play beta), Mac: install OpenDisplay Receiver from
  https://github.com/peetzweg/opendisplay/releases/latest."
- **CPU warning is buried.** A 5K iMac means software-encoding 4096x2304;
  Known issues says so, but the Install page does not. A laptop user will hit
  fan noise and 55-70 ms and think it is broken. Suggested one line in the
  support table's Mac receiver cell: "Mac receivers are encoded on the CPU at
  up to 4096x2304; expect high CPU use and ~60 ms latency."
- **Success:** the iMac shows a Retina desktop and System Settings > Display
  lists it. **Give up:** `KWin does not offer zkde_screencast_unstable_v1 ...`
  because the binary is in `~/.local/bin`; the error text does tell them to
  install the package, but there is no Fedora package, so it is a dead end
  without the steps above.

### 4. Steam Deck / SteamOS tinkerer

Not mentioned anywhere, and they will try. SteamOS is Arch-based but has a
read-only root, no `yay`, and Desktop Mode runs Plasma 5.27 (6.x on newer
images); Gaming Mode runs gamescope.

- **Gaming Mode:** detect falls through to "no supported compositor found
  (unknown desktop); od-sender supports Hyprland >= 0.55 for now" (detect.rs).
  That message also understates the beta (it names only Hyprland while the
  README lists four backends). Suggested: "no supported compositor found
  (<desktop>). od-sender creates displays on Hyprland 0.55+, Sway 1.10+, KDE
  Plasma and GNOME 50+; on other Wayland compositors pass `--output <name>`.
  See <link>."
- **Desktop Mode on Plasma 5.27:** the code gate is "Plasma 5.24+" (zkde v2 /
  fake input v3) with the desktop-entry rule, so it could work, but the
  `/usr/bin/od-sender` requirement collides with the read-only root. A
  `.desktop` in `~/.local/share/applications` whose `Exec=` points at
  `/home/deck/.local/bin/od-sender` should satisfy KWin's check. Suggested
  README note under KDE: "SteamOS (Desktop Mode): put `od-sender` in
  `~/.local/bin`, copy `od-sender.desktop` to `~/.local/share/applications/`
  with `Exec=` changed to that path, re-login. Gaming Mode is not supported."
  Mark it untested.
- **glibc:** SteamOS 3.6+ has glibc 2.38+, so the tarball runs. Say so, or
  say "untested".
- **Success:** the Deck's desktop shows on their iPad in Desktop Mode.
  **Give up:** `yay: command not found`, then the KWin error above.

### 5. Arch packager / AUR maintainer reading the PKGBUILD

- **Version ordering bug.** `pkgver=0.1.0_beta.1`: on this machine
  `vercmp 0.1.0_beta.1 0.1.0` returns `1`, i.e. pacman considers the beta
  *newer* than the final `0.1.0`. Users on the beta will never be offered
  `0.1.0` without an `epoch` bump. `vercmp 0.1.0beta1 0.1.0` returns `-1`
  (correct). Suggested: `pkgver=0.1.0beta1` with `_tag="linux-v0.1.0-beta.1"`
  derived via a small `_ver`/`_pre` split in the PKGBUILD and `bump.sh`
  (adjust the `s/-/_/` mapping to `s/-//; s/\.\([0-9]*\)$/\1/` or keep an
  explicit `_tag=` variable). Fix before the AUR package is first published;
  renaming later costs an epoch forever.
- **`sha256sums=('SKIP')` in the committed PKGBUILD and .SRCINFO.** Fine in
  the repo (bump.sh fills it), but the AUR must never receive SKIP for a
  release tarball. Add a check in `bump.sh` (`grep -q SKIP PKGBUILD && exit 1`
  after `updpkgsums`).
- **`provides=('od-sender')`.** `provides` names packages, not binaries;
  nothing else will depend on a package called `od-sender`. Harmless, but a
  reviewer will flag it; drop it or comment why.
- **`check()` runs `cargo test --release -p od-sender`.** Doubles compile for
  dev-dependencies; acceptable, but note the time, and verify in CI that the
  unit tests really need no Wayland socket (the comment claims it).
- **`makedepends=('cargo' ...)` plus `export RUSTUP_TOOLCHAIN=stable`.** Correct
  per Arch Rust guidelines; `rustc --print host-tuple` needs Rust 1.84+, fine
  on Arch.
- **GitHub archive tarballs** are acceptable but note they exclude
  submodules; there are none. `--frozen` requires `linux/Cargo.lock` to be
  committed; it is.
- **`options=('!lto')` with a comment**: good. `depends` listing `gcc-libs`
  and `glibc`: fine.
- **Desktop entry:** `Icon=video-display` (theme icon, fine),
  `Categories=Utility;` fine; `Terminal=true` fine for a CLI. The
  `X-KDE-Wayland-Interfaces` line is documented in a comment; good.
- **User unit:** `Restart=on-failure` + `RestartSec=5` with a 10 s discovery
  window means a unit with the phone off retries every ~15 s forever and
  fills the journal with `no receiver found within 10s` at ERROR level. Suggest
  `RestartSec=15` and logging the discovery timeout at WARN when running under
  systemd, or a `--wait` mode that keeps browsing. `WantedBy=graphical-session.target`
  works on Omarchy (the target is active and WAYLAND_DISPLAY /
  HYPRLAND_INSTANCE_SIGNATURE are imported here); a plain `exec Hyprland` from a
  TTY without uwsm does not start that target. Mention the `exec-once`
  alternative in the README for non-Omarchy Hyprland.
- **Verdict:** publishable after the pkgver fix and SKIP guard; the rest is
  nits.

### 6. Tech journalist reading the website and release notes

- **Headline mismatch.** The hero says "as your Mac's second monitor"; the
  Linux sender is a one-line addition to the tagline and a card. That is
  honest for a beta, but the title tag and OG text never say Linux, so the
  story they write is "Mac tool adds experimental Linux support". If the goal
  is a Linux story, add Linux to `<title>` and `og:description`.
- **"Tested on Omarchy / Hyprland 0.55+" on the card vs "Tested: Hyprland
  0.56.x, aarch64" in the README vs "Hyprland 0.56 on aarch64" in the release
  notes.** A journalist will quote the card. Align all three to the README's
  wording.
- **The release notes draft is good and quotable:** concrete latency numbers
  (iPhone ~20 ms, Android 33 ms median, iMac 55-70 ms), a clear tier table, a
  frank Known issues list. Two things they will ask: (1) hardware encode: the
  site's feature grid says "Hardware H.264 or HEVC (VideoToolbox)" and the
  Linux docs say software only; say "CPU-encoded on Linux for now (VA-API
  planned)" on the card. (2) Security: the privacy page says "the one current
  caveat about unencrypted WiFi transport" for Mac; the Linux sender is WiFi
  only, so that caveat applies to 100% of Linux sessions. Add Linux to the
  privacy page's scope line ("applies to the OpenDisplay Mac, iOS and Android
  apps") and one sentence in the Linux README: "The stream is not encrypted;
  use a network you trust."
- **No Linux screenshot or clip.** The Showcase is Mac. One 10-second clip of
  an Omarchy desktop appearing on an iPhone is the single most-shared asset.
- **Version string.** Mac is `v0.x.y`, Linux is `linux-v0.1.0-beta.1`, the
  release title is "Linux sender 0.1.0-beta.1", the release notes heading is
  "OpenDisplay Linux sender 0.1.0 beta 1". Pick "OpenDisplay Linux sender
  0.1.0 beta 1" and use it in the title too.
- **"Linux receiver (preview)" next to "Linux sender (beta)"** is clear on the
  site and correctly explained. Good.

---

## Part 2: release-lead view

### Is the tested / expected / experimental / unsupported messaging honest?

Mostly yes, and better than most betas: four tiers, named versions, "never
run on a real desktop" stated outright, and `doctor` exits 2 with a reason.
Gaps:

1. The site card and FAQ collapse "tested" to "Omarchy / Hyprland 0.55+" and
   drop the architecture; x86_64 has never run (§8.8). Quote from App.tsx:
   "Tested on Omarchy / Hyprland 0.55+." Replace with: "Tested on Omarchy /
   Hyprland 0.56 on Apple silicon (Asahi); x86_64 expected to work."
2. The README's "Tested" row mentions Hyprland 0.56.x while the intro says
   "tested on Omarchy / Hyprland 0.55 or newer". Make the intro say "0.56
   (0.55 expected to work)".
3. Android: release notes say "expected to work" with measured numbers; fine,
   but finish the five-item checklist before the tag or state what is
   unchecked.
4. The detect fallback message names only Hyprland ("od-sender supports
   Hyprland >= 0.55 for now"). Update to the four backends plus `--output`.
5. KDE crash-recovery wording contradicts between README and release notes
   (see persona 3).

### Install paths

- **AUR (source build):** correct path for Omarchy. Unstated build time and
  dependency weight; measure on x86_64 and aarch64 and print both. The
  `pkgver` ordering bug must be fixed before first publish.
- **Tarballs:** glibc 2.38+ and libpipewire are the only runtime needs; the
  README omits libpipewire (the workflow notes include it). The bundled
  `.desktop` and unit hardcode `/usr/bin`, so they are decoration for tarball
  users; document the sed or ship `/usr/local/bin` paths. Give explicit
  `install` commands.
- **From source:** the Arch line is good; add Fedora/Ubuntu equivalents
  (`dnf install rust cargo ffmpeg-devel pipewire-devel clang pkgconf` with the
  RPM Fusion caveat; `apt install rustup libavcodec-dev libavformat-dev
  libswscale-dev libpipewire-0.3-dev clang pkgconf`), since those are exactly
  the users told to build from source until the tarballs exist.

### Naming consistency

| Where | Name |
|---|---|
| binary / commands | `od-sender` |
| AUR package | `opendisplay-sender` |
| desktop entry Name | "OpenDisplay Sender" |
| unit Description | "OpenDisplay sender to %I" |
| release title | "Linux sender 0.1.0-beta.1" |
| release notes H2 | "OpenDisplay Linux sender 0.1.0 beta 1" |
| README H1 | "OpenDisplay sender for Linux (beta)" |
| thing it creates | "virtual output" (README) / "virtual display" (release notes, website) / "monitor" (Hyprland) |

Acceptable: `od-sender` for the command, `opendisplay-sender` for the
package, "OpenDisplay Sender" for humans. Fix: one product phrase ("OpenDisplay
Linux sender") for titles, and one noun for the thing it creates. Recommend
"virtual display" in every user-facing text, with "(an output, in Hyprland's
terms)" once.

### First-run experience (`doctor`, `list`, `run --to`)

- `doctor` is strong: version, detection steps, protocols with versions,
  encoders, locks, config echo with errors, Hyprland permission state. Keep it
  the first command in every doc (the release notes do; the README's Usage
  block lists `run` first; swap).
- `list` with nothing found prints `no receivers found` and exits 1 with no
  hint; `run` fails with `no receiver found within 10s`. Add the hint text
  from persona 2 to both.
- `run` with several receivers picks one silently. Either refuse without
  `--to` when more than one is found, or log the choice loudly.
- `run` should print one plain-language line on success, e.g. `extending onto
  "iPhone" (2556x1179 @3x) as display od-iphone, right of DP-1; Ctrl+C to
  stop`. Today the equivalent is `plan: ...` at INFO plus backend chatter.
- `--help` exposes `dev` ("Developer self-tests") at top level; hide it
  (`hide = true`) so the first `--help` a user sees has four verbs.
- The legacy `--connect` / `--name` note in the README ("replaces the ...
  options of the pre-beta builds") is noise for beta users who never saw
  pre-beta builds; move it to a changelog.

### Docs gaps (README)

1. Troubleshooting section: "no receiver found", "black display on the
   device" (Hyprland permissions, already there but not under a findable
   heading), "display appears but taps do nothing", "high CPU".
2. iOS/Android receiver prerequisites (Local Network permission, app in
   foreground) on the Linux page; today only the Mac README has them.
3. Uninstall.
4. Tarball desktop entry / unit path fix.
5. Fedora KDE 6.7 exact steps; SteamOS note.
6. `--workspace` is in the options table but the config example and the
   options table disagree on scope wording ("Hyprland and Sway; ignored with a
   warning elsewhere" vs "Hyprland/Sway: show workspace 8"); fine, but say
   "ignored with a warning on KDE and GNOME" in both places.
7. Security line: WiFi only, unencrypted.
8. Link the tracking issue (#299) from the Install section, not only the
   intro and the end.

### What must be true before announcing

1. Tag `linux-v0.1.0-beta.1` exists, `linux-release.yml` has produced both
   tarballs, and a fresh Omarchy x86_64 machine has run `yay -S
   opendisplay-sender` to completion (CI builds the PKGBUILD; a human has not,
   on x86_64, per §8.8).
2. AUR package published with a real sha256 and a `pkgver` that sorts below
   `0.1.0`.
3. Website, README, FAQ and structured data switched from "coming" to live in
   one commit, deployed after the release exists (the site reads
   `releases/latest`; the pre-release is `--latest=false`, so the Linux card
   needs its own link to the `linux-v*` release).
4. One x86_64 session logged (any receiver), so the "Expected to work" row can
   at least say "x86_64: one tester confirmed".
5. Android checklist (touch, scroll, rotation, lock, quit) done or listed as
   unchecked.
6. The KDE 6.7 tarball recipe tried once on a real Plasma (even a VM), since
   most Plasma users are there.
7. Tracking issue #299 open with a report template (doctor output, log command,
   versions) matching the README's "Reporting a problem".

### Launch checklist (short)

- [ ] Fix `pkgver` scheme; run `bump.sh`; confirm no `SKIP`; push AUR.
- [ ] Tag; verify both tarballs and `.sha256`; run `od-sender doctor` from a
      tarball on Ubuntu 24.04+ and Fedora 39+ (glibc claim).
- [ ] Fix tarball `.desktop` / unit paths or document the edit.
- [ ] Align "tested" wording across card, FAQ, index.html JSON-LD, README,
      release notes; add architecture.
- [ ] Flip "coming soon" to live (App.tsx card + FAQ, index.html FAQ JSON-LD
      and featureList, README Linux section); link the `linux-v*` release.
- [ ] Add Troubleshooting + Uninstall + receiver prerequisites to the README.
- [ ] Improve the three error strings: no receiver found, unsupported
      compositor, several receivers.
- [ ] Record one short Linux clip for the Showcase and the release post.
- [ ] Privacy page: add Linux to scope; README: unencrypted WiFi line.
- [ ] Post the release notes draft with `gh release edit --notes-file`, then
      announce (#299, X, Omarchy community).
