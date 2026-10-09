#!/bin/sh
# SUBJECT: A bound binary constructor retains its selected prefix inside a higher-kinded newtype argument.
# CONTRACT: specs/backends/ts.md § Higher-kinded types
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-bound-partial.XXXXXX")
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
    --rootDir . --outDir ../host-build consumer.mts
  node ../host-build/consumer.mjs
)
