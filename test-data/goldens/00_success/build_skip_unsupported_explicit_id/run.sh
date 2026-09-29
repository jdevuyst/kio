#!/bin/sh
# Best-effort skipping never applies to a target explicitly named by the user.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/golden.skip-unsupported-explicit.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir/package"
set +e
output=$("$KIO_BIN" build wasm --skip-unsupported-targets 2>&1 >/dev/null)
status=$?
set -e
case "$output" in
  *"warning: skipping target"*)
    printf 'unexpected skip warning for an explicit target: %s\n' "$output" >&2
    exit 1
    ;;
esac
if [ "$status" -ne 40 ]; then
  printf 'expected exit 40, got %s\n' "$status" >&2
  exit 1
fi
