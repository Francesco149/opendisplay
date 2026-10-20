# OpenDisplay sender for Linux (beta)

`od-sender` extends a Linux desktop onto an OpenDisplay receiver: an iPhone,
iPad, Android device or spare Mac running the OpenDisplay app. It creates a
virtual output on your compositor sized for the receiver, captures it,
encodes H.264 and streams it over WiFi. Taps and two-finger scrolls on the
receiver come back as pointer input, and rotating the device rebuilds the
output in portrait.

It speaks the same protocol as the Mac sender ([PROTOCOL.md](../PROTOCOL.md)),
so the receivers from the App Store, Google Play and the releases page work
unchanged. There is no account, no server and no telemetry: one TCP
connection between the two devices.

The beta targets one desktop: **Omarchy / Hyprland 0.55 or newer**. GNOME, KDE
and Sway are planned ([#299](https://github.com/peetzweg/opendisplay/issues/299)).

## Supported setups

| | Setup |
|---|---|
| **Tested** | Arch / Omarchy, Hyprland 0.56.x, aarch64 (Asahi Linux on an M1 Pro), software x264 encode. Receivers: iPhone (App Store), Android (Google Play beta), Mac receiver app (Intel iMac) |
| **Expected to work** | Arch / Omarchy on x86_64 (also x264; VA-API is not used yet). Any distro with Hyprland 0.55+ and an FFmpeg built with libx264 (Fedora's `ffmpeg-free` falls back to OpenH264, which is slower). iPad as receiver |
| **Not supported yet** | Sway and other wlroots compositors, niri, COSMIC, river, labwc, GNOME, KDE, X11 sessions, Hyprland older than 0.55, USB cable transport, HEVC, Apple Pencil pressure (mapped to a plain pointer), audio, mirror mode |

On an unsupported desktop `od-sender` exits with a message that names what is
missing. If you run an "expected to work" setup, a report that it works (or
doesn't) helps: see [Reporting a problem](#reporting-a-problem).

## Install

### Arch / Omarchy (AUR)

```sh
yay -S opendisplay-sender
```

This installs `/usr/bin/od-sender`, a desktop entry (it shows up in the
Omarchy menu and opens in a terminal) and a `systemd --user` unit,
`od-sender@.service`, for starting it on login.

Prebuilt `x86_64` and `aarch64` tarballs are attached to the `linux-v*`
pre-releases on the [releases page](https://github.com/peetzweg/opendisplay/releases).
They are built on Ubuntu 24.04 against its FFmpeg; on Arch, use the AUR
package.

### From source

You need Rust (stable), FFmpeg with its headers, Wayland client libraries,
`pkgconf` and `clang` (for the FFmpeg bindings). On Arch:

```sh
sudo pacman -S --needed rust ffmpeg wayland pkgconf clang
git clone https://github.com/peetzweg/opendisplay
cd opendisplay/linux
cargo build --release -p od-sender
./target/release/od-sender doctor
```

## Usage

Open the OpenDisplay app on the receiver, on the same network as your
computer, then:

```sh
od-sender run                 # discover receivers and connect to the first one found
od-sender run --to iPhone     # pick one by name, receiver id, or address (host:port)
od-sender list                # show receivers on the network: name, device, protocol, address
od-sender doctor              # explain what od-sender will use on this desktop
od-sender clean               # remove virtual outputs left behind by a crashed sender
```

`run` discovers receivers over Bonjour/mDNS for up to 10 seconds. The new
output appears to the right of your current monitors; stop the sender with
Ctrl+C and the output is removed. If the WiFi drops, the sender reconnects.

Useful `run` options:

| Option | Default | |
|---|---|---|
| `--position` | `auto-right` | Where the output goes: `auto-right`, `auto-left`, `auto-up`, `auto-down`, or `x,y` |
| `--scale` | from the receiver | Output scale |
| `--bitrate-mbps` | `20` | Video bitrate |
| `--max-fps` | receiver's panel rate | Frame-rate cap |
| `--encoder` | `auto` | `auto`, `x264`, `v4l2m2m` or `openh264` |
| `--no-cursor` | off | Don't paint the cursor into the video |

> **Note:** `--to` replaces the `--connect <addr>` and `--name <substring>`
> options of the pre-beta builds. If you built from source before the beta,
> use `--to` with either an address or a name.

### Configuration

Defaults for the options above, plus per-receiver overrides, go in
`~/.config/opendisplay/sender.toml`. Command-line options win over the file.

```toml
bitrate-mbps = 20
position = "auto-right"

# Overrides for one receiver, keyed by its id (shown by `od-sender list`).
[receiver."3F2A9C1E-..."]
position = "auto-left"
scale = 2
```

### Start on login

```sh
systemctl --user enable --now od-sender@iPhone.service
```

The instance name is passed to `od-sender run --to`, so it can be a receiver
name, id or address.

## Hyprland permissions

Omarchy does not enable Hyprland's permission system by default, and then
nothing needs configuring. If you turned on `ecosystem:enforce_permissions`,
Hyprland does not refuse an unapproved screen capture: it hands the client
black frames with a "permission denied" caption, so the receiver shows a black
display. Either allow the capture in the dialog Hyprland shows, or add a rule
for `od-sender` to `~/.config/hypr/hyprland.lua`:

```lua
hl.permission("/usr/bin/od-sender", "screencopy", "allow")
```

The first argument is a regex for the binary's path; use your own path for a
source build (for example `".*/od-sender"`). Hyprland reads permission rules
only at startup, so restart Hyprland after adding it. Touch and scroll go
through the virtual pointer protocol, which Hyprland does not gate. Run
`od-sender doctor` to see whether permissions are enforced on your session.

## Known issues

* **The cursor is part of the video.** It moves with video latency, and on a
  static screen it can lag until something else on the output changes. Use
  `--no-cursor` to leave it out. A separate cursor channel is planned.
* **Software encoding.** Hardware encode (VA-API, V4L2) is not used yet, so
  encoding runs on the CPU with x264. Large receivers, such as a 5K iMac
  (streamed at 4096x2304), cost noticeably more CPU than a phone.
* **High latency to the Mac receiver.** Streaming to the Mac receiver on an
  Intel iMac has shown about 220 ms end to end at 4096x2304, far above the
  ~20 ms seen with an iPhone. The cause is under investigation; picture and
  input are correct.
* **Hyprland only.** See [Supported setups](#supported-setups).

## Reporting a problem

[Open an issue](https://github.com/peetzweg/opendisplay/issues/new/choose)
with:

1. The output of `od-sender doctor`.
2. A debug log of the failing session:
   ```sh
   RUST_LOG=debug od-sender run --to <receiver> 2>&1 | tee od-sender.log
   ```
3. Your distro, Hyprland version (`hyprctl version`), CPU architecture, and
   the receiver device and app version.

## Development

The Rust workspace in this directory also holds a Linux receiver
(`od-receiver`, not part of the beta) and the protocol crates. See
[PLAN.md](PLAN.md) for the overall plan and [SENDER-PLAN.md](SENDER-PLAN.md)
for the sender's road to this beta, including how to test without risking
your own Hyprland session.
