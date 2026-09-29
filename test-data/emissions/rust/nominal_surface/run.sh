#!/bin/sh
# SUBJECT: Rust hosts use explicit, label-generated, roleless-payload, and same-leaf nominal types through declaration-keyed public paths.
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
