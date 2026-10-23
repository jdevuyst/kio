#!/bin/sh
# `flatten_sum!` rewrites only the outer-axis position
# (`Sum(Sum(L1, L2), R) → Sum(L1, Sum(L2, R))`); structure inside
# a right slot is left untouched. Source `(A | B) | (C | D)` and
# target `(A | B) | (D | C)` have the same fully-flattened arm
# multiset `{A, B, C, D}` and the same `assoc_depth = 1`, so the
# Cat 1 and Cat 3 checks both pass. But the inner sum `C | D` vs
# `D | C` differs by permutation, which `flatten_sum!` cannot
# touch — the user would need `reorder_sum!` on that slot. Per
# `specs/formal/elaborator.md` § 12.4.1 case 2 (right-shape mismatch),
# the elaborator rejects with the inner-slot diagnostic.
# `expected.stderr` is pinned to lock the spec phrasing.
# Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
