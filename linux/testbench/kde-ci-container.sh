#!/usr/bin/env bash
# Runs inside `docker run --device /dev/dri -v <checkout>:/src -v <logs>:/logs
# archlinux:latest` (linux.yml, kde-smoke): installs KWin and the build deps,
# builds od-sender and a null-sink od-receiver, then kde-smoke.sh as a user.
set -euo pipefail

pacman -Syu --noconfirm --needed \
  base-devel git rust clang pkgconf nasm ffmpeg libpipewire \
  kwin pipewire wireplumber dbus
useradd --create-home tester
chown -R tester: /src /logs
# The render node (vgem on hosted runners) belongs to the host's groups.
chmod a+rw /dev/dri/* || true
ls -l /dev/dri

sudo -u tester -H bash -c '
  set -euo pipefail
  cd /src/linux
  cargo build --locked --release -p od-sender
  cargo build --locked --release -p od-receiver --no-default-features
'
sudo -u tester -H env OD_RECEIVER=/src/linux/target/release/od-receiver \
  timeout 240 /src/linux/testbench/kde-smoke.sh /src/linux/target/release/od-sender /logs
