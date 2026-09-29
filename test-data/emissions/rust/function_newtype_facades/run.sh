#!/bin/sh
# SUBJECT: Rust hosts instantiate rank-N function newtypes with declaration-owned and external native constructor witnesses while incompatible storage retags panic in safe Rust.
# CONTRACT: specs/backends/rust.md § Representation witness API, safety, and coherence; § Higher-kinded types; § Polymorphic newtype payloads
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
cargo rustc --offline --quiet --manifest-path "$scratch/workdir/out/rust/Cargo.toml" --lib -- -Funsafe-code
cargo run --offline --quiet --manifest-path "$scratch/host/Cargo.toml"
