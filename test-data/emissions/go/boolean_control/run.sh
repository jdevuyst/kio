#!/bin/sh
# SUBJECT: Canonical inline Boolean operands lower to native Go operators or statements without branch closures, retaining opaque callback invocation.
# CONTRACT: evidence/backend-performance.md § Boolean operand recovery; shared strict Boolean selection.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-boolean-control.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp inspect.py "$scratch/inspect.py"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
python3 "$scratch/inspect.py" out/go/pkg.go
