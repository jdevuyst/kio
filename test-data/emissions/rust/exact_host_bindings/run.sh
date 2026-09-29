#!/bin/sh
# SUBJECT: The Rust facade accepts exact module-qualified host bindings, role bounds, and public callback aliases.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM

cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

cp "$scratch/host/manifest.toml" "$scratch/host/Cargo.toml"
cargo run --offline --quiet --manifest-path "$scratch/host/Cargo.toml"
