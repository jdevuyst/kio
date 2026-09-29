#!/bin/sh
# Subject: widen_prod! — the diagonal (specs/language.md § Spine-
# based elaborator palette, specs/formal/elaborator.md § 12.4 — R-Diag-Prod +
# R-Identity-Prod-intro).
#
# Pre-condition: for every type U ≠ (), multiplicity in target ≥
# multiplicity in source; `.`-typed target slots are unconstrained.
# Two rules cooperate: R-Diag-Prod duplicates a source slot when the
# target has more slots of that type than the source provides (the
# "duplicate the first" fallback under source-order pinning);
# R-Identity-Prod-intro synthesizes `__unit__` for `.`-typed target
# slots that have no source counterpart.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
