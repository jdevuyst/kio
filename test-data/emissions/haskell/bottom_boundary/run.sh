#!/bin/sh
# SUBJECT: Haskell exposes Kio bottom as Data.Void.Void throughout its public host boundary.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.bottom-boundary.XXXXXX")
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
