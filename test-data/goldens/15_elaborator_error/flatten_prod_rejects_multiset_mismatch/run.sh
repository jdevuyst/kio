#!/bin/sh
# `flatten_prod!` preserves the fully-flattened factor multiset
# exactly — R-Assoc-Prod is the associativity rewrite and leaves
# are invariant under it. Source `(A & B) & C` has leaves
# `{A, B, C}`; target `A & C` has leaves `{A, C}` — `B` is
# missing. Per `specs/formal/elaborator.md` § 12.4.1 case 1
# (multiset mismatch), the elaborator rejects with the
# multiset-focused diagnostic — steer toward `narrow_prod!`
# (to drop `B`) or `fit!` (to compose narrow with flatten).
# `expected.stderr` is pinned to lock the spec phrasing.
# Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
