#!/bin/sh
# Subject: flatten_sum! — iterative outer-axis associativity on the
# sum axis (specs/language.md § Spine-based elaborator palette, specs/
# formal/elaborator.md § 12.4.1 — R-Assoc-Sum oriented left-to-right).
#
# Pre-condition: source and target sum-typed; target required (no
# shallow-flatten path); μ(spine_sum(S)) = μ(spine_sum(T)) (R-Assoc
# preserves arm multisets exactly); assoc_depth(S) ≥ assoc_depth(T).
# Rule fires R-Assoc-Sum iteratively (k = assoc_depth(S) -
# assoc_depth(T) steps) rewriting `Sum(Sum(L1, L2), R)` to
# `Sum(L1, Sum(L2, R))` at the outer sum until shapes match. Each
# step is an `__either__` dispatch re-injecting through `__left__`
# / `__right__` at the new shape. The arm multiset is preserved.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
