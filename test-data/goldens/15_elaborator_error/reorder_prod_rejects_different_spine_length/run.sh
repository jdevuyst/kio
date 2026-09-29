#!/bin/sh
# `reorder_prod!` is pure permutation — source and target spines
# must have the same length. Source `(A & B)` (2 slots), target
# `(A & B & C)` (3 slots) — different spine lengths, so reorder
# rejects. To extend use `widen_prod!`. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
