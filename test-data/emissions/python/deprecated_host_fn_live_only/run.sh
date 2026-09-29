#!/bin/sh
# SUBJECT: A Python host supplies only current bindings after sealed host-function removals and reaches the live export.
# CONTRACT: specs/backends/python.md § Deprecated host items
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-deprecated-host.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

PYTHONPATH="$scratch/workdir/out/python" \
  python3 "$scratch/host/host_driver.py"
