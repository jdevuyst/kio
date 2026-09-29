#!/bin/sh
# SUBJECT: Haskell escapes a structural product family whose preferred public name collides with a nominal carrier.
# CONTRACT: specs/backends/haskell.md, Item naming, Declaration-local type names.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.structural-nominal-collision.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"
mkdir -p "$scratch/ghc"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
ghc -v0 -fforce-recomp -fno-code \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/Host.hs"
