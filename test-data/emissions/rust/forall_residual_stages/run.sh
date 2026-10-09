#!/bin/sh
# SUBJECT: Rust hosts name, implement, retain, and reuse exact one-binder residuals independently of later type and function applications.
# CONTRACT: specs/backends/rust.md § Higher-rank value parameters
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust-forall-residuals.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
rustc --edition=2024 -F unsafe-code --crate-name rust_forall_residual_stages \
  --crate-type=rlib "$scratch/workdir/out/rust/src/lib.rs" \
  -o "$scratch/librust_forall_residual_stages.rlib"
rustc --edition=2024 "$scratch/host/main.rs" \
  --extern "rust_forall_residual_stages=$scratch/librust_forall_residual_stages.rlib" \
  -o "$scratch/host-probe.exe"
"$scratch/host-probe.exe"
