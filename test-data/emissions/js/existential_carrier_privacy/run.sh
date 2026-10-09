#!/bin/sh
# SUBJECT: A JavaScript host can open an existential only through its staged public projector and cannot inspect or cross-instance forge its carrier.
# CONTRACT: specs/backends/js.md § Structural and nominal types
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.js-carrier-privacy.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
node "$scratch/host/consumer.mjs"
