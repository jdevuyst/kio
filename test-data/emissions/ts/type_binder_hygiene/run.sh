#!/bin/sh
# SUBJECT: Source generic names preserve exact callable, higher-kinded, and nested public TypeScript relationships.
# CONTRACT: specs/backends/ts.md § FFI surface
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-binders.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
(
  cd "$scratch/host"
  tsc --strict --target ES2022 --module NodeNext --moduleResolution NodeNext \
    --rootDir . --outDir ../host-build consumer.ts
  node ../host-build/consumer.js
)
