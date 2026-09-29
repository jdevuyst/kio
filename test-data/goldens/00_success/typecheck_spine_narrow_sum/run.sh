#!/bin/sh
# Subject: narrow_sum! — codiagonal collapse on the sum axis
# (specs/language.md § Spine-based elaborator palette, specs/formal/
# elaborator.md § 12.4 — R-Comm + R-Collapse-Sum + R-Identity-Sum-elim).
#
# Pre-condition: for every non-`!` type U, U ∈ μ(spine_sum(T)) ⟺
# U ∈ μ(spine_sum(S)) (type-set equality on non-`!` types), and
# target multiplicity ≤ source multiplicity. Two rules cooperate:
# R-Collapse-Sum forwards like-typed source arms to a single target
# arm (the codiagonal); R-Identity-Sum-elim drops source `!` arms
# freely.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
