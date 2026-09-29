#!/bin/sh
# Subject: narrow_prod! — product-axis truncation (specs/language.md
# § Spine-based elaborator palette, specs/formal/elaborator.md § 12.4 —
# R-Project-Prod + R-Identity-Prod-elim).
#
# Pre-condition: μ(spine_prod(T)) ⊆ μ(spine_prod(S)) as multisets.
# The rule fires R-Project-Prod (a `__fst__` / `__snd__` chain to
# the kept slots, source-order pinned). R-Identity-Prod-elim is a
# special case: a `.`-typed source slot drops via the same chain.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
