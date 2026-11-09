#!/bin/sh
# SUBJECT: Canonical pure Boolean macros/operators use native Haskell operators without branch lambdas, while effectful branches retain native if.
# CONTRACT: evidence/backend-performance.md § Boolean operand recovery; shared strict Boolean selection.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.haskell-boolean-control.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp inspect.py "$scratch/inspect.py"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
python3 "$scratch/inspect.py" out/haskell/BooleanControl.hs
