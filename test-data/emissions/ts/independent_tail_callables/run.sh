#!/bin/sh
# SUBJECT: Strict TS independently generated facades exchange precise cooperative generic stages and nested results.
# CONTRACT: specs/backends/ts.md § Carve-outs
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-independent-tail.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  cd peer
  "$KIO_BIN" build "$KIO_TARGET"
)
(
  cd "$scratch/host"
  tsc --strict --target ES2022 --module NodeNext --moduleResolution NodeNext \
    --rootDir . --outDir ../host-build consumer.ts
  node ../host-build/consumer.js
)
