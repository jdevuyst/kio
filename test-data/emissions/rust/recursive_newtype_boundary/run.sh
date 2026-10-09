#!/bin/sh
# SUBJECT: Rust hosts construct and project mutually recursive nominal carriers through exact Tree/Forest signatures and public marker facades.
# CONTRACT: specs/backends/rust.md § Structural and nominal types; § Representation witness API, safety, and coherence; § Newtype namespaces
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust-recursive-boundary.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
rustc --edition=2024 -F unsafe-code --crate-name rust_recursive_newtype_boundary \
  --crate-type=rlib "$scratch/workdir/out/rust/src/lib.rs" \
  -o "$scratch/librust_recursive_newtype_boundary.rlib"
rustc --edition=2024 "$scratch/host/main.rs" \
  --extern "rust_recursive_newtype_boundary=$scratch/librust_recursive_newtype_boundary.rlib" \
  -o "$scratch/host-probe.exe"
"$scratch/host-probe.exe"
