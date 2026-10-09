#!/bin/sh
# SUBJECT: An invariant TypeScript host container preserves direct and substituted product/sum types and their complete payloads.
# CONTRACT: specs/backends/ts.md § Non-atomic host types
# Compound right tails must commute with generic instantiation beneath opaque Box.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-opaque-substitution.XXXXXX")
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
