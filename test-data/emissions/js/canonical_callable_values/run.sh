#!/bin/sh
# SUBJECT: Independently emitted JavaScript callable carriers compose stages and deep adapters while preserving concrete snapshots, opaque transport, and nominal checks.
# CONTRACT: specs/backends/js.md § Carve-outs
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.js-callable-values.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
(
  cd "$scratch/workdir/foreign"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
node "$scratch/host/consumer.mjs"
