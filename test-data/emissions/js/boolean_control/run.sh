#!/bin/sh
# SUBJECT: Canonical Boolean macros and operators use native control without delayed inline branch functions; opaque callbacks and eager factories retain their stages.
# CONTRACT: evidence/backend-performance.md § Boolean operand recovery; shared strict Boolean selection.
set -eu

if command -v mise >/dev/null 2>&1; then
  tsc_path=$(MISE_ENABLE_TOOLS="${MISE_ENABLE_TOOLS:+$MISE_ENABLE_TOOLS,}npm:typescript" mise which tsc)
else
  tsc_path=$(command -v tsc)
fi
scratch=$(mktemp -d "${TMPDIR:?}/emissions.js-boolean-control.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp inspect.cjs "$scratch/inspect.cjs"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/inspect.cjs" out/js/boolean_control.js "$tsc_path"
