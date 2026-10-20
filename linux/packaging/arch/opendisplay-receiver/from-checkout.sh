#!/usr/bin/env bash
# Builds the package from this checkout's HEAD instead of the release tag:
# `git archive` stands in for GitHub's tag tarball (same top-level directory),
# so the PKGBUILD runs unmodified. Commit first; the worktree is not included.
# Extra arguments go to makepkg, e.g. `./from-checkout.sh --nocheck`.
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=PKGBUILD
source PKGBUILD
tarball="${pkgname}-${pkgver}.tar.gz"
git -C ../../../.. archive --format=tar.gz --prefix="opendisplay-${_tag}/" -o "${PWD}/${tarball}" HEAD
# The local tarball is not the tag's, so its checksum cannot match.
SRCDEST="${PWD}" makepkg --force --cleanbuild --skipchecksums "$@"
