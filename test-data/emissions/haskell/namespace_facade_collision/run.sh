#!/bin/sh
# SUBJECT: Haskell's public facade remains importable when its configured namespace is Type.
# CONTRACT: specs/backends/haskell.md, Output layout and Loading protocol.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.namespace-facade-collision.XXXXXX")
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
