#!/bin/sh
# SUBJECT: Haskell exposes polymorphic nominal carriers through its documented public facade.
set -eu

case_dir=$(pwd)
scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
cp -R "$case_dir/host" "$scratch/host"
rm -rf "$scratch/out"
mkdir -p "$scratch/ghc"

cd "$scratch"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
ghc -v0 -fforce-recomp -fno-code \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/Host.hs"
