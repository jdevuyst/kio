#!/bin/sh
# SUBJECT: Haskell requires a host-type equation for every live declaration in a wide public contract.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.wide-host-types.XXXXXX")
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
  "$scratch/host/FullHost.hs"
missing_log=$scratch/ghc/MissingHost.stderr
if ghc -v0 -fforce-recomp -fno-code \
  -odir "$scratch/ghc" \
  -hidir "$scratch/ghc" \
  -iout/haskell \
  "$scratch/host/MissingHost.hs" >"$scratch/ghc/MissingHost.stdout" 2>"$missing_log"; then
  printf 'wide_host_type_contract: missing T64 equation unexpectedly compiled\n' >&2
  exit 1
fi
missing_identity='api/T64'
if ! grep -Fq "$missing_identity" "$missing_log"; then
  printf 'wide_host_type_contract: missing-host fixture failed for an unrelated reason\n' >&2
  cat "$missing_log" >&2
  exit 1
fi
