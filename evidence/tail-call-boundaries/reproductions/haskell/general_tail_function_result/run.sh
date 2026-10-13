#!/bin/sh
# SUBJECT: A reusable public forall stage returns a Unit-domain callable without retaining one result frame per captured tail link.
# CONTRACT: specs/backends/haskell.md § FFI surface
# BUG: Generated callable-result adapters retain tail frames before the type stage returns.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.haskell-tail-function-result.XXXXXX")
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
