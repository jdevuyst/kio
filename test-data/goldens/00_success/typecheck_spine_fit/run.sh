#!/bin/sh
# Subject: fit! — the recursive composer, function-arrow-aware
# (specs/language.md § Spine-based elaborator palette, specs/formal/
# elaborator.md § 12.6 — WK-FitFnFn, WK-FitProd, WK-FitSum, WK-FitNarrow
# BeforeWiden, WK-FitLeaf, WK-FitStrictInitial).
#
# fit! walks the structure of source and target and composes
# `narrow_sum!`, `narrow_prod!`, `widen_sum!` per the narrow-before-
# widen discipline. Function arrows recurse contravariantly on the
# parameter and covariantly on the return. `!` source at any depth
# is the strict-initial carve-out — elaborates via `__absurd__`.
# fit! **excludes** widen_prod! (no diagonal duplication) — that's
# its defining no-duplication identity.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
