#!/bin/sh
# SUBJECT: Haskell native IO and rank-N public callables retain reusable deep captured chains under the same bounded RTS stack in both optimization modes.
# CONTRACT: specs/backends/haskell.md § FFI surface
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.haskell-tail-calls.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
for optimization in 0 2; do
  mkdir -p "$scratch/ghc-$optimization"
  ghc -v0 -fforce-recomp -rtsopts "-O$optimization" \
    -odir "$scratch/ghc-$optimization" -hidir "$scratch/ghc-$optimization" \
    -iout/haskell -main-is Host.main -o "$scratch/host-check" "$scratch/host/Host.hs"
  printf 'O%s: ' "$optimization"
  "$scratch/host-check" +RTS -K1M -RTS
done
