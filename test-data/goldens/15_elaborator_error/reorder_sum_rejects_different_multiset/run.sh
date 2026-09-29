#!/bin/sh
# `reorder_sum!` requires source and target spines to have
# identical multisets of arm types. Source `(A | B)`, target
# `(A | A)` — different multisets (B vs second A), so reorder
# rejects. To change the multiset use `narrow_sum!` / `widen_sum!`.
# Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
