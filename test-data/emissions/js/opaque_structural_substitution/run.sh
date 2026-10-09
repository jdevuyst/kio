#!/bin/sh
# SUBJECT: An independent JavaScript host extracts a complete structural payload from its opaque generic container.
# CONTRACT: specs/backends/js.md § Non-atomic host types
# The generic host result must not be mistaken for a private body representation.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.js-opaque-substitution.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
node "$scratch/host/consumer.mjs"
