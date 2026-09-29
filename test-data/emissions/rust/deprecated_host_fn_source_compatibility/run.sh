#!/bin/sh
# SUBJECT: Rust compiles unchanged and live-only hosts while retained defaults stay outside live dispatch.
set -eu

case_dir=$(pwd)
scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
rm -rf "$scratch/out"
cp -R "$case_dir/host" "$scratch/retained-host"
mkdir -p "$scratch/cargo-target" "$scratch/rust-tmp"

cd "$scratch"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
(
  cd retained-host
  CARGO_TARGET_DIR="$scratch/cargo-target" \
    TMPDIR="$scratch/rust-tmp" \
    cargo run --offline --quiet
)
