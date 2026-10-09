#!/bin/sh
# SUBJECT: Rust public forall constructors defer host effects and exceptions until each application, including nested and multi-binder values.
# CONTRACT: specs/backends/rust.md § Higher-rank value parameters; specs/backends/README.md § 9. Call-by-value evaluation order
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust-forall-stages.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
rustc --edition=2024 -F unsafe-code --crate-name rust_forall_constructor_stages \
  --crate-type=rlib "$scratch/workdir/out/rust/src/lib.rs" \
  -o "$scratch/librust_forall_constructor_stages.rlib"
rustc --edition=2024 "$scratch/host/main.rs" \
  --extern "rust_forall_constructor_stages=$scratch/librust_forall_constructor_stages.rlib" \
  -o "$scratch/host-probe.exe"
"$scratch/host-probe.exe"
