#!/bin/sh
# The two `role(bool)` literal spellings `.t` and `.f`
# residualize to distinct literal values that disagree, per
# specs/formal/equiv.md § 4.5 "literals that disagree: .t ≄
# .f". The literal comparison is its own branch of the
# equivalence relation (a `Bool` NF, not an atom or stuck call).
# Exits 50 from the `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
