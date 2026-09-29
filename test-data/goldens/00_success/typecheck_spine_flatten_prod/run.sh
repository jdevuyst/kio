#!/bin/sh
# Subject: flatten_prod! — iterative outer-axis associativity on the
# product axis (specs/language.md § Spine-based elaborator palette,
# specs/formal/elaborator.md § 12.4.1 — R-Assoc-Prod oriented left-to-
# right).
#
# Pre-condition: source and target product-typed; target required;
# μ(spine_prod(S)) = μ(spine_prod(T)); assoc_depth(S) ≥
# assoc_depth(T). Rule fires R-Assoc-Prod iteratively (k =
# assoc_depth(S) - assoc_depth(T) steps), rewriting
# `Prod(Prod(L1, L2), R)` to `Prod(L1, Prod(L2, R))` at the outer
# product until shapes match. Each step rebuilds via `__pair__`
# from `__fst__` / `__snd__` projections at the new shape. The
# factor multiset is preserved.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
