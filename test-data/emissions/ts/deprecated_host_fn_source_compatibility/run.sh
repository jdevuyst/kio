#!/bin/sh
# SUBJECT: TypeScript keeps representable removed host declarations optional so strict old and live-only hosts both compile.
# CONTRACT: specs/backends/ts.md § Deprecated host items
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-deprecated-host.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"
mkdir -p "$scratch/host-check"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

(
  cd "$scratch/host"
  TMPDIR="$scratch/host-check" \
    tsc \
      --strict \
      --noEmit \
      --target es2020 \
      --module nodenext \
      --moduleResolution nodenext \
      old_host.ts \
      new_host.ts
)
