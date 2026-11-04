#!/usr/bin/env bash
# Build omafeed-VERSION-ARCH-linux.tar.gz from the checkout SOURCE into OUTPUT.
#
# Runs as root in a debian:13 container. Debian 13 is the oldest system the
# DEB and RPM packages support, so the binary's glibc and library floors are
# its own; the same archive also runs on Arch Linux and Fedora.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 SOURCE OUTPUT" >&2
  exit 2
fi
source=$(realpath "$1")
output=$2

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends build-essential ca-certificates curl git pkg-config \
  libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev
# A fixed compiler, so a rebuild of a tag gives the same binary. Raise it
# deliberately; RUST_TOOLCHAIN overrides it for a one-off build.
toolchain=${RUST_TOOLCHAIN:-1.99.1}
curl --proto '=https' --tlsv1.2 --retry 3 -sSf https://sh.rustup.rs \
  | sh -s -- -y --profile minimal --default-toolchain "$toolchain"
export PATH="$HOME/.cargo/bin:$PATH"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$source/Cargo.toml" | head -n1)
test -n "$version" || { echo "no version in $source/Cargo.toml" >&2; exit 1; }

make -C "$source" build
stage=$(mktemp -d)
make -C "$source" install DESTDIR="$stage" PREFIX=/usr
# Loading the binary resolves every library it links, without a display.
test "$("$stage/usr/bin/omafeed" --version)" = "omafeed $version"

mkdir -p "$output"
tar -C "$stage" --owner=0 --group=0 --numeric-owner \
  -czf "$output/omafeed-$version-$(uname -m)-linux.tar.gz" usr
