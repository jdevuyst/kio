#!/bin/sh
# SUBJECT: Independently generated JS artifacts exchange cooperative value and executing type stages with nested callable results.
# CONTRACT: specs/backends/js.md § Cooperative callable protocol
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.js-independent-tail.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  cd peer
  "$KIO_BIN" build "$KIO_TARGET"
)
node "$scratch/host/consumer.mjs"
