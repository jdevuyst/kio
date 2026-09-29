#!/bin/sh
# SUBJECT: A strict independently authored TypeScript host uses live HKT, rank-N, existential, and recursive facade relationships.
# CONTRACT: specs/backends/ts.md § Higher-kinded types
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-rich-host.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

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
