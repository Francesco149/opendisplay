#!/usr/bin/env bash
# Points the package at a new release tag once it is pushed:
#   ./bump.sh 0.1.0-beta.2      (or linux-v0.1.0-beta.2)
# Sets pkgver (dashes become underscores) and pkgrel=1, downloads the tag
# tarball from GitHub to record its sha256, and regenerates .SRCINFO.
set -euo pipefail
cd "$(dirname "$0")"
ver="${1:?usage: $0 <version, e.g. 0.1.0-beta.2>}"
ver="${ver#linux-v}"
pkgver="${ver//-/_}"
sed -i -e "s/^pkgver=.*/pkgver=${pkgver}/" -e "s/^pkgrel=.*/pkgrel=1/" PKGBUILD
# A tarball left by from-checkout.sh has the same name; hash the real one.
rm -f "opendisplay-receiver-${pkgver}.tar.gz"
SRCDEST="${PWD}" updpkgsums
makepkg --printsrcinfo > .SRCINFO
git diff --stat -- PKGBUILD .SRCINFO
