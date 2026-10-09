#!/bin/sh
# SUBJECT: Independently emitted JavaScript callables preserve type stages, shared structural values, opaque transport, and authorized nominal payload access.
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
