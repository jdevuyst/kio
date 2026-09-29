#!/bin/sh
# SUBJECT: Haskell scopes literal constraints to the exact host type and the functions that use it.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.scoped-role-constraints.XXXXXX")
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
  "$scratch/host/SafeHost.hs" \
  "$scratch/host/ConstrainedHost.hs"
