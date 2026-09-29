#!/bin/sh
# SUBJECT: A Rust host implements exact role bounds, owned strings, marker-polymorphic methods, and the public package factory.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
cargo run --offline --quiet --manifest-path "$scratch/host/Cargo.toml"
