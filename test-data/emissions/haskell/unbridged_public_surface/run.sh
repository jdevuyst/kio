#!/bin/sh
# SUBJECT: Haskell's public facade exposes bridged modules without leaking public declarations from unbridged modules.
# CONTRACT: specs/backends/haskell.md, Package facade and Exported-function wrappers.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.unbridged-public-surface.XXXXXX")
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
hidden_log=$scratch/ghc/HiddenHost.stderr
if ghc -v0 -fforce-recomp -fno-code \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/HiddenHost.hs" >"$scratch/ghc/HiddenHost.stdout" 2>"$hidden_log"; then
  printf 'unbridged_public_surface: hidden export unexpectedly compiled\n' >&2
  exit 1
fi
hidden_identity=export__hidden__hiddenPair
if ! grep -Fq "$hidden_identity" "$hidden_log"; then
  printf 'unbridged_public_surface: hidden-host fixture failed for an unrelated reason\n' >&2
  cat "$hidden_log" >&2
  exit 1
fi
