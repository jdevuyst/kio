#!/bin/sh
# `widen_prod!` requires the source's non-`()` factor-type multiset
# to be ≤ the target's: every non-`()` slot type the source has,
# the target must have at least as many of. Source `(A & B)`,
# target `(A & C)` — source has a `B` slot but the target has no
# `B`, so widen_prod! cannot synthesize a slot of an absent type.
# The form duplicates source slots (R-Diag-Prod) or synthesizes
# `__unit__` for `()` slots (R-Identity-Prod-intro) — neither
# fabricates a `B`. Reach for `narrow_prod!` (drop `B`) composed
# with widen_prod! (add `C` as `.`-typed), or use `fit!` for the
# cross-axis reshape. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
