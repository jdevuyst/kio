#!/bin/sh
# `flatten_prod!` is oriented left-to-right (`Prod(Prod(L1, L2), R)
# → Prod(L1, Prod(L2, R))`). The inverse rewrite (re-nesting) is
# not in scope. Source `A & (B & C)` has `assoc_depth = 0`
# (outer-left factor is `A`, a leaf); target `(A & B) & C` has
# `assoc_depth = 1` (outer-left is the product `A & B`). `k =
# depth(source) - depth(target) = -1`, so the source is *shallower*
# than the target requires. Per `specs/formal/elaborator.md` § 12.4.1
# case 3 (shallow source), the elaborator rejects with the
# depth-focused diagnostic. `expected.stderr` is pinned to lock
# the spec phrasing. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
