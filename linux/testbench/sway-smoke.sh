#!/usr/bin/env bash
# Capture + input smoke test of od-sender on a headless Sway: no GPU, no
# Hyprland, nothing touches the caller's session (own XDG_RUNTIME_DIR, so the
# sender can only reach this Sway). Used by CI (linux.yml, sway-smoke) and
# runnable locally:
#
#   linux/testbench/sway-smoke.sh <od-sender> [<logdir>]
#
# Needs sway (>= 1.10 for ext-image-copy-capture), foot, wlr-randr, jq.
#
# 1. Create an output with `swaymsg create_output`, size it through
#    zwlr_output_manager_v1 (wlr-randr --custom-mode), which is how a future
#    wlroots backend would size its own output.
# 2. Two foot windows on it, the left one redrawing constantly (capture only
#    yields frames on damage).
# 3. `od-sender doctor`, for the log: what the sender makes of this desktop.
# 4. `od-sender dev smoke --backend adopt --output`: the sender's own
#    backend path (adopt, capture, input, encode, teardown) on that output.
# 5. `od-sender dev capture-test`: at least MIN_FRAMES frames and a saved
#    frame of the right size.
# 6. `od-sender dev input-test --output`: the virtual pointer, bound to that
#    output, moves to x=0.25 then 0.75; Sway's focus-follows-mouse must move
#    focus to the left then right window, which proves absolute motion lands
#    on the right output at the right place (dev smoke cannot check that).
#
# TODO(SENDER-PLAN §4.3): once `dev smoke` grows `--connect`, stream to
# `od-receiver --listen 127.0.0.1:9100 --sink none` and check its stats; and
# switch 4 to `--backend wlroots` (creating its own output) when that exists.
set -euo pipefail

sender="$(realpath "${1:?usage: $0 <od-sender> [<logdir>]}")"
logdir="$(realpath -m "${2:-sway-smoke-logs}")"
MIN_FRAMES="${MIN_FRAMES:-30}"
mkdir -p "$logdir"

# Short path: Wayland and Sway IPC socket paths are limited to 108 bytes.
runtime="$(mktemp -d /tmp/od-sway.XXXXXX)"
chmod 700 "$runtime"
export XDG_RUNTIME_DIR="$runtime"
unset WAYLAND_DISPLAY SWAYSOCK HYPRLAND_INSTANCE_SIGNATURE DISPLAY
export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
export LANG="${LANG:-C.UTF-8}"

cat > "$runtime/sway.conf" <<'EOF'
xwayland disable
default_border none
focus_follows_mouse yes
EOF

sway_pid=
cleanup() {
  [ -n "$sway_pid" ] && kill "$sway_pid" 2>/dev/null && wait "$sway_pid" 2>/dev/null
  rm -rf "$runtime"
}
trap cleanup EXIT

sway -c "$runtime/sway.conf" > "$logdir/sway.log" 2>&1 &
sway_pid=$!
for _ in $(seq 50); do
  sock="$(ls "$runtime"/sway-ipc.*.sock 2>/dev/null || true)"
  [ -n "$sock" ] && [ -S "$runtime/wayland-1" ] && break
  sleep 0.1
done
[ -n "$sock" ] || { echo "sway did not start"; cat "$logdir/sway.log"; exit 1; }
export SWAYSOCK="$sock" WAYLAND_DISPLAY=wayland-1
swaymsg -t get_version -r | jq -r '"sway " + .human_readable'

outputs() { swaymsg -t get_outputs -r | jq -r '.[].name' | sort; }
before="$(outputs)"
swaymsg -q create_output
out="$(comm -13 <(echo "$before") <(outputs))"
[ -n "$out" ] || { echo "create_output made no output"; exit 1; }
wlr-randr --output "$out" --custom-mode 640x480@60Hz
mode="$(swaymsg -t get_outputs -r | jq -r --arg o "$out" '.[] | select(.name == $o) | .current_mode | "\(.width)x\(.height)@\(.refresh)"')"
echo "output $out: $mode"
[ "$mode" = "640x480@60000" ] || { echo "custom mode not applied"; exit 1; }

swaymsg -q focus output "$out"
windows() { swaymsg -t get_tree -r | jq -r '.. | select(.app_id? == "left" or .app_id? == "right") | .app_id'; }
# One at a time: Sway tiles in map order, and the left window must be left.
spawn() {  # spawn <app-id> <command>
  swaymsg -q "exec foot -c /dev/null --app-id $1 -- sh -c '$2'"
  for _ in $(seq 50); do
    windows | grep -qx "$1" && return
    sleep 0.1
  done
  echo "foot window $1 did not map"; exit 1
}
spawn left 'while :; do date +%s.%N; sleep 0.005; done'
spawn right 'sleep 600'
focused() { swaymsg -t get_tree -r | jq -r '.. | select(.focused? == true) | .app_id // .name'; }

echo "--- doctor"
"$sender" doctor 2>&1 | tee "$logdir/doctor.log" || true

echo "--- dev smoke (adopt)"
"$sender" dev smoke --backend adopt --output "$out" --size 640x480 --frames 30 2>&1 |
  tee "$logdir/smoke.log"
grep -q "smoke ok" "$logdir/smoke.log" || { echo "dev smoke did not pass"; exit 1; }

echo "--- capture"
"$sender" dev capture-test --output "$out" --seconds 3 --save "$logdir/frame.ppm" 2>&1 |
  tee "$logdir/capture.log"
frames="$(sed -n 's/.* \([0-9]\+\) frames captured.*/\1/p' "$logdir/capture.log")"
[ "${frames:-0}" -ge "$MIN_FRAMES" ] || { echo "captured ${frames:-0} frames, want >= $MIN_FRAMES"; exit 1; }
header="$(head -c 15 "$logdir/frame.ppm" | tr '\n' ' ')"
case "$header" in "P6 640 480 "*) ;; *) echo "saved frame header: $header"; exit 1 ;; esac

echo "--- input"
# Sway ignores the first pointer motion after it starts for focus purposes
# (it only seeds the cursor position; later motions from fresh virtual
# pointers do move focus), so warm up once without checking.
"$sender" dev input-test --output "$out" --x 0.5 --y 0.9 --hold 0.3 2>&1 | tee -a "$logdir/input.log"
for step in "0.25 left" "0.75 right" "0.25 left"; do
  read -r x want <<< "$step"
  "$sender" dev input-test --output "$out" --x "$x" --y 0.5 --hold 0.3 2>&1 | tee -a "$logdir/input.log"
  got="$(focused)"
  echo "pointer at x=$x: focused $got"
  [ "$got" = "$want" ] || { echo "expected $want focused"; exit 1; }
done

echo "--- teardown"
swaymsg -q output "$out" unplug
outputs | grep -qx "$out" && { echo "$out still present after unplug"; exit 1; }
kill -0 "$sway_pid" || { echo "sway exited"; exit 1; }
echo "sway smoke passed: $frames frames, input on $out"
