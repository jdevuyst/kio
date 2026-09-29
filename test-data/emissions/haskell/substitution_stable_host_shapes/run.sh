#!/bin/sh
# SUBJECT: Haskell public boundary shapes remain equal under product, sum, rank-N, alias, newtype, and HKT substitution.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.substitution-stable-shapes.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"
mkdir -p "$scratch/ghc"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
ghc -v0 -fforce-recomp \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  -main-is Host.main \
  -o "$scratch/host-check" \
  "$scratch/host/Host.hs"
"$scratch/host-check"
