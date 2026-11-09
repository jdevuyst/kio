#!/bin/sh
# SUBJECT: Boolean selection preserves exact host objects and their tags through inline operands, opaque callbacks, and eager thunk factories.
# CONTRACT: specs/backends/python.md § Host record contract; shared Boolean operand semantics.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-boolean-carrier.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
PYTHONPATH="$scratch/workdir/out/python" python3 "$scratch/host/driver.py"
