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

The beta is tested on **Omarchy / Hyprland 0.55 or newer**. Sway, KDE Plasma
and GNOME have their own backends, marked **experimental**: each passes an
automated test on a headless copy of that compositor, but none has run on a
real desktop session yet. Reports are very welcome
([#299](https://github.com/peetzweg/opendisplay/issues/299)).

## Supported setups

| | Setup |
|---|---|
| **Tested** | Arch / Omarchy, Hyprland 0.56.x, aarch64 (Asahi Linux on an M1 Pro), software x264 encode. Receivers: iPhone (App Store), Mac receiver app (Intel iMac) |
| **Expected to work** | Arch / Omarchy on x86_64 (also x264; VA-API is not used yet). Any distro with Hyprland 0.55+ and an FFmpeg built with libx264 (Fedora's `ffmpeg-free` falls back to OpenH264, which is slower). iPad and Android (Google Play beta) as receivers |
| **Experimental** | **Sway 1.10+**, **KDE Plasma 6.8+** (6.7 and older only with the packaged desktop entry, see [KDE Plasma](#kde-plasma)), **GNOME 50+** (Mutter 50; older GNOME may work, untested). Verified on headless compositors only |
| **Existing outputs only** | niri, COSMIC, river, labwc, Wayfire and similar: `od-sender run --output <name>` streams an output you created yourself, if the compositor offers the screen-capture and virtual-pointer protocols `od-sender doctor` lists |
| **Not supported yet** | X11 sessions, Hyprland older than 0.55, Flatpak, USB cable transport, HEVC, Apple Pencil pressure (mapped to a plain pointer), audio, mirror mode |

On an unsupported desktop `od-sender` exits with a message that names what is
missing. If you run an "expected to work" or experimental setup, a report that
it works (or doesn't) helps: see [Reporting a problem](#reporting-a-problem).

## Install

### Arch / Omarchy (AUR)

```sh
yay -S opendisplay-sender
```

This installs `/usr/bin/od-sender`, a desktop entry (it shows up in the
Omarchy menu and opens in a terminal) and a `systemd --user` unit,
`od-sender@.service`, for starting it on login.

### Other distributions

Prebuilt `x86_64` and `aarch64` tarballs are attached to the `linux-v*`
pre-releases on the [releases page](https://github.com/peetzweg/opendisplay/releases).
They link a minimal FFmpeg and x264 statically, so no FFmpeg install is
needed; they run on glibc 2.38 or newer (Ubuntu 24.04, Debian 13, Fedora 39
and later). Unpack and put `od-sender` on your `PATH`. Each tarball also
carries the desktop entry and the `systemd --user` unit.

### From source

You need Rust (stable), FFmpeg and PipeWire with their headers, `pkgconf`
and `clang` (for the FFmpeg bindings). On Arch:

```sh
sudo pacman -S --needed rust ffmpeg libpipewire pkgconf clang
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
od-sender list                # receivers on the network: name, address, protocol version, id
od-sender doctor              # explain what od-sender will use on this desktop
od-sender clean               # remove virtual outputs left behind by a crashed sender
```

`run` discovers receivers over Bonjour/mDNS for up to 10 seconds; `--to`
matches part of a receiver's name or its full id, or connects straight to a
`host:port`. `od-sender` detects your desktop (Hyprland, Sway, KDE Plasma or
GNOME) and creates a virtual output there; `--backend` overrides the choice.
The new output appears to the right of your current monitors.
If the WiFi drops, the sender reconnects, waiting a little longer after each
failed attempt (up to 10 seconds). If the output is removed from under it
(`hyprctl output remove`, a monitor reconfiguration), the sender recreates
it; after three losses within a minute it gives up and exits.

Stop the sender with Ctrl+C, `SIGTERM` (what `systemctl --user stop` sends)
or `SIGHUP` (closing its terminal): each removes the output before exiting.
After a crash or `kill -9` the output stays behind on Hyprland and Sway;
`od-sender clean` removes it, and the next `run` for the same receiver reuses
it. KDE Plasma and GNOME remove it themselves when the sender dies.

Only one sender can drive a receiver at a time. A second `run` for the same
receiver exits with `another od-sender (pid N) is already driving …`.

Useful `run` options:

| Option | Default | |
|---|---|---|
| `--to` | first receiver found | Receiver name, id, or `host:port` |
| `--position` | `auto-right` | Where the output goes: `auto-right`, `auto-left`, `auto-up`, `auto-down`, or `x,y` |
| `--scale` | `passthrough` | Output scale: `passthrough` (the receiver's own, 3 on most iPhones), `mac` (2 for Retina-class screens, else 1, like the Mac sender) or a number. GNOME rounds to the nearest scale it offers for that size (an iPhone's 3 becomes 2) |
| `--bitrate-mbps` | `20` | Video bitrate |
| `--max-fps` | receiver's panel rate | Frame-rate cap |
| `--encoder` | `auto` | `auto`, `v4l2m2m`, `x264` or `openh264` |
| `--cursor` | `auto` | `auto`, `video` (always paint the cursor into the video) or `off`; `--no-cursor` is `off`. `auto` sends the cursor separately when the receiver and desktop support it, which today means never on Hyprland |
| `--capture` | `auto` | Screen-capture protocol on Hyprland and Sway: `auto`, `ext` or `wlr`. `auto` uses `wlr` on Hyprland before 0.57, whose `ext` capture can crash when an output disappears |
| `--backend` | detected | `hyprland`, `wlroots` (Sway), `kde`, `gnome`, or `adopt` with `--output <name>` |

`--instance <index|signature>` (any command) targets one of several running
Hyprland instances, like `hyprctl -i`; `od-sender doctor` lists them.

> **Note:** `--to` replaces the `--connect <addr>` and `--name <substring>`
> options of the pre-beta builds; both are still accepted for now.

### `doctor`

`od-sender doctor` prints how it detected your desktop, the Hyprland version
and permission setting, the Wayland protocols it found, which encoders open,
and your config file. It changes nothing. Exit code 0 means `run` can work
here; 2 means an unsupported desktop or a config file that does not parse
(the output says which); 1 is any other error, such as an unknown
`--instance`.

### Configuration

Defaults for `run`'s options, plus overrides per receiver, go in
`~/.config/opendisplay/sender.toml` (`$XDG_CONFIG_HOME` is honoured; pass
`--config <path>` to use another file). Keys are the option names:

```toml
to = "iPhone"            # used when run gets no --to
position = "auto-right"
scale = "passthrough"    # "passthrough", "mac" or a number
bitrate-mbps = 20
max-fps = 60
encoder = "auto"         # "auto", "v4l2m2m", "x264" or "openh264"
cursor = "auto"          # "auto", "video" or "off" (true/false: auto/off)
capture = "auto"         # "auto", "ext" or "wlr"

# Overrides for one receiver: position, scale and bitrate-mbps only.
[receiver."3F2A9C1E-..."]
position = "auto-left"
scale = 2
```

A `[receiver."…"]` table is matched against the receiver's id (shown by
`od-sender list`, case does not matter), then its network name, then the
device name it reports. When several settings apply, the command line wins,
then the receiver's table, then the top of the file, then the built-in
defaults.

Unknown keys and invalid values are errors: `run` refuses to start and
`od-sender doctor` shows the problem, rather than a typo being silently
ignored.

### Start on login

```sh
systemctl --user enable --now od-sender@iPhone.service
```

The instance name is passed to `od-sender run --to`, so it can be a receiver
name, id or address.

## Desktop notes

### Sway

Sway 1.10 or newer. `od-sender` creates a headless output with
`swaymsg create_output`, sizes and places it through the output-management
protocol, and removes it on exit; `od-sender clean` removes one left by a
crash.

### KDE Plasma

KDE Plasma 6.8 or newer lets `od-sender` create a virtual output and move the
pointer with no dialog. **Plasma 6.7 and older** only grant that to a program
named by an installed desktop entry, so there it works with the packaged
`/usr/bin/od-sender` (the AUR package and the tarballs ship
`od-sender.desktop`) but not with a build run from elsewhere. Scale and
position are set like on the other desktops. Plasma does not allow outputs at
negative coordinates, so `--position auto-left` or `auto-up` puts the new
output at the top left and shifts your other screens right or down; they move
back when the sender exits, but not after a crash: then rearrange them in
System Settings → Display, or start the sender again, which lines them up.

### GNOME

GNOME 50 or newer (Mutter 50). `od-sender` uses Mutter's own screen-cast and
remote-desktop interfaces, so there is no sharing dialog. To set the scale and
position it applies a temporary display configuration that keeps all your
other monitors exactly as they are, and GNOME restores your layout when the
virtual display goes away. Not available from a Flatpak.

### Hyprland permissions

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

* **The cursor is part of the video on Hyprland.** It moves with video
  latency, and on a static screen it can lag until something else on the
  output changes. Hyprland's cursor-capture interface cannot deliver the
  cursor image yet, so the separate cursor channel stays off there. Use
  `--cursor off` to leave it out.
* **Software encoding.** Hardware encode (VA-API, V4L2) is not used yet, so
  encoding runs on the CPU with x264. Large receivers, such as a 5K iMac
  (streamed at 4096x2304), cost noticeably more CPU than a phone.
* **Large receivers lag more.** A 5K iMac streamed at 4096x2304 shows about
  55-70 ms end to end, against ~20 ms for an iPhone: the time goes into
  software encoding the larger picture.
* **Sway, KDE Plasma and GNOME are untested on real desktops.** See
  [Supported setups](#supported-setups).

## Reporting a problem

[Open an issue](https://github.com/peetzweg/opendisplay/issues/new/choose)
with:

1. The output of `od-sender doctor`.
2. A debug log of the failing session:
   ```sh
   RUST_LOG=debug od-sender run --to <receiver> 2>&1 | tee od-sender.log
   ```
3. Your distro, desktop and its version (`hyprctl version`, `sway --version`,
   `plasmashell --version` or `gnome-shell --version`), CPU architecture, and
   the receiver device and app version.

## Development

The Rust workspace in this directory also holds a Linux receiver
(`od-receiver`, not part of the beta) and the protocol crates. See
[PLAN.md](PLAN.md) for the overall plan and [SENDER-PLAN.md](SENDER-PLAN.md)
for the sender's road to this beta, including how to test without risking
your own Hyprland session.
