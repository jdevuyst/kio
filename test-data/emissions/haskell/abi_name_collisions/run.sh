#!/bin/sh
# SUBJECT: Haskell public names typecheck and distinct affix-path exports execute their matching host fields.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.abi-name-collisions.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"
mkdir -p "$scratch/ghc"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
ghc -v0 -fforce-recomp -main-is Host.main \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/Host.hs" \
  -o "$scratch/host-main"
"$scratch/host-main"
