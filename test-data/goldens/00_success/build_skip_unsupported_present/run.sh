#!/bin/sh
# Best-effort implicit selection warns for an unsupported target while still
# building the package's supported targets successfully.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/golden.skip-unsupported.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
output=$("$KIO_BIN" build --skip-unsupported-targets 2>&1 >/dev/null)
case "$output" in
  *"warning: skipping target 'wasm': no backend in this kio build"*) ;;
  *)
    printf 'expected the wasm skip warning, got: %s\n' "$output" >&2
    exit 1
    ;;
esac
