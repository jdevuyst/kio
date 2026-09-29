#!/bin/sh
# `flatten_sum!` is oriented left-to-right: `Sum(Sum(L1, L2), R)`
# is the *source* shape and `Sum(L1, Sum(L2, R))` is the result.
# The inverse rewrite (re-nesting) is not in scope. Source
# `A | (B | C)` has `assoc_depth = 0` (outer-left arm is `A`, a
# leaf); target `(A | B) | C` has `assoc_depth = 1` (outer-left
# is the sum `A | B`). `k = depth(source) - depth(target) = -1`,
# so the source is *shallower* than the target requires. Per
# `specs/formal/elaborator.md` § 12.4.1 case 3 (shallow source) and
# § 12.10.13's canonical example, the elaborator rejects with the
# depth-focused diagnostic. `expected.stderr` is pinned to lock
# the spec phrasing. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
