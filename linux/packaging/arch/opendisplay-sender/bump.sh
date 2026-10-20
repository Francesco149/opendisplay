#!/usr/bin/env bash
# Points the package at a new release tag once it is pushed:
#   ./bump.sh 0.1.0-beta.2      (or linux-v0.1.0-beta.2)
# Sets pkgver (tag 0.1.0-beta.2 -> 0.1.0beta2, see the PKGBUILD), _tag and
# pkgrel=1, downloads the tag tarball from GitHub to record its sha256, and
# regenerates .SRCINFO. Refuses to finish with a SKIP checksum: the AUR must
# never receive one for a release tarball.
#   ./bump.sh --check-versions  checks the mapping sorts right under vercmp
set -euo pipefail
cd "$(dirname "$0")"

# 0.1.0 -> 0.1.0; 0.1.0-beta.1 -> 0.1.0beta1; 1.2.3-rc.10 -> 1.2.3rc10.
pkgver_for() {
  local v="${1#linux-v}"
  if [[ ! "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$ ]]; then
    echo "bump.sh: '$1' is not X.Y.Z or X.Y.Z-(alpha|beta|rc).N" >&2
    return 1
  fi
  if [[ "$v" == *-* ]]; then
    local base="${v%%-*}" pre="${v#*-}"
    echo "${base}${pre%%.*}${pre#*.}"
  else
    echo "$v"
  fi
}

check_versions() {
  local fail=0 ok a b want got
  # Each pair must sort a < b under pacman.
  for pair in \
    0.1.0-alpha.1:0.1.0-beta.1 0.1.0-beta.1:0.1.0-beta.2 0.1.0-beta.2:0.1.0-beta.10 \
    0.1.0-beta.10:0.1.0-rc.1 0.1.0-rc.1:0.1.0 0.1.0:0.1.1-beta.1 0.1.1-beta.1:0.1.1 \
    0.9.0:0.10.0-beta.1; do
    a=$(pkgver_for "${pair%%:*}"); b=$(pkgver_for "${pair##*:}")
    ok=1
    for want in "-1:$a:$b" "1:$b:$a"; do
      got=$(vercmp "$(cut -d: -f2 <<<"$want")" "$(cut -d: -f3 <<<"$want")")
      if [[ "$got" != "${want%%:*}" ]]; then
        echo "FAIL vercmp $(cut -d: -f2- <<<"$want" | tr : ' ') = $got, want ${want%%:*}" >&2
        fail=1 ok=0
      fi
    done
    [[ $ok == 1 ]] && echo "ok   ${pair%%:*} ($a) < ${pair##*:} ($b)"
  done
  for bad in 0.1 0.1.0-beta 0.1.0_beta.1 v0.1.0 0.1.0-dev.1; do
    if pkgver_for "$bad" >/dev/null 2>&1; then
      echo "FAIL '$bad' was accepted" >&2
      fail=1
    fi
  done
  return $fail
}

if [[ "${1:-}" == "--check-versions" ]]; then
  check_versions
  exit
fi

ver="${1:?usage: $0 <version, e.g. 0.1.0-beta.2> | --check-versions}"
ver="${ver#linux-v}"
pkgver="$(pkgver_for "$ver")"
sed -i -e "s/^pkgver=.*/pkgver=${pkgver}/" -e "s/^_tag=.*/_tag=linux-v${ver}/" \
  -e "s/^pkgrel=.*/pkgrel=1/" PKGBUILD
# A tarball left by from-checkout.sh has the same name; hash the real one.
rm -f "opendisplay-sender-${pkgver}.tar.gz"
SRCDEST="${PWD}" updpkgsums
if grep -q "SKIP" PKGBUILD; then
  echo "bump.sh: PKGBUILD still has a SKIP checksum; is linux-v${ver} pushed?" >&2
  exit 1
fi
makepkg --printsrcinfo > .SRCINFO
git diff --stat -- PKGBUILD .SRCINFO
