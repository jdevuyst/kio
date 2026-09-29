#!/bin/sh
# Regression: alias-aware carrier-cycle detection must see a mutual
# newtype recursion (Even <-> Odd) hidden behind non-parametric type
# aliases, so the Haskell native renderer promotes a carrier instead of
# expanding the alias body forever, and the Rust walk erases the
# recursive leaf.
set -u
cd workdir || exit
"$KIO_BIN" build "$KIO_TARGET" || exit

if [ "$KIO_TARGET" = rust ]; then
  ( cd out/rust && cargo check --offline --quiet ) || {
    printf 'emitted Rust crate failed cargo check\n' >&2
    exit 1
  }
fi
