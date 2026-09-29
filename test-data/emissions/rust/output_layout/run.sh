#!/bin/sh
# SUBJECT: The Rust target emits the specified self-contained Cargo crate layout and Rust 2024 manifest.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/workdir"
rm -rf "$scratch/workdir/out"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

for path in Cargo.toml src/lib.rs src/host.rs src/shapes.rs src/ffi.rs src/__kio_runtime.rs; do
  test -f "out/rust/$path"
done
grep -qF 'name = "rust_output_layout"' out/rust/Cargo.toml
grep -qF 'edition = "2024"' out/rust/Cargo.toml
grep -qF 'pub trait RustOutputLayoutHost' out/rust/src/host.rs
grep -qF 'pub fn create_rustOutputLayout' out/rust/src/lib.rs
(
  cd out/rust
  cargo check --offline --quiet
)
