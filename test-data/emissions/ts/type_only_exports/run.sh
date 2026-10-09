#!/bin/sh
# SUBJECT: TypeScript hosts specialize type-only exports and use their exact returned callable types.
# CONTRACT: specs/backends/ts.md § Package API
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-type-only-exports.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
(
  cd "$scratch/host"
  tsc --strict --noEmit --target es2020 --module nodenext --moduleResolution nodenext consumer.ts
)
