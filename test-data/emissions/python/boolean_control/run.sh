#!/bin/sh
# SUBJECT: Canonical Boolean macro/operator bodies serialize direct conditional children, with no inline branch function objects and retained opaque callback calls.
# CONTRACT: evidence/backend-performance.md § Boolean operand recovery; shared strict Boolean selection.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-boolean-control.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp inspect.py "$scratch/inspect.py"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
python3 "$scratch/inspect.py" out/python
