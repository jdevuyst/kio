#!/bin/sh
# SUBJECT: Rust callable and forall facades reuse deep tail/pending chains and release pending frame values on completion or panic on a bounded host stack in every thread-safety mode.
# CONTRACT: specs/backends/README.md § Calls with work remaining; specs/backends/rust.md § Thread safety; specs/backends/rust.md § Higher-rank value parameters
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.rust-tail-calls.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

for mode in local send sync send_sync; do
  setting=$mode
  if [ "$mode" = local ]; then
    setting='()'
  fi
  sed "s/thread_safety ()/thread_safety $setting/" \
    workdir/rust_general_tail_calls.pkg.kio \
    >"$scratch/workdir/rust_general_tail_calls.pkg.kio"
  (
    cd "$scratch/workdir"
    "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  )
  for optimization in 0 2; do
    rustc --edition=2024 -F unsafe-code -C "opt-level=$optimization" \
      --crate-name rust_general_tail_calls --crate-type=rlib \
      "$scratch/workdir/out/rust/src/lib.rs" \
      -o "$scratch/librust_general_tail_calls.rlib"
    set --
    if [ "$mode" != local ]; then
      set -- --cfg thread_safe
    fi
    rustc --edition=2024 -F unsafe-code -C "opt-level=$optimization" "$@" \
      "$scratch/host/main.rs" \
      --extern "rust_general_tail_calls=$scratch/librust_general_tail_calls.rlib" \
      -o "$scratch/host-probe.exe"
    printf '%s O%s: ' "$mode" "$optimization"
    "$scratch/host-probe.exe"
  done
done
