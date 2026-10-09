#!/bin/sh
# SUBJECT: Haskell split and packed parameters have distinct native types, including callbacks, newtype members, Unit and staged effects.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.source-parameters.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
mkdir -p "$scratch/ghc"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
ghc -v0 -fforce-recomp -odir "$scratch/ghc" -hidir "$scratch/ghc" \
  -iout/haskell -main-is Host.main -o "$scratch/host-check" "$scratch/host/Host.hs"
"$scratch/host-check"
