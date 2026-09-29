#!/bin/sh
# Subject: widen_sum! — sum-axis capacity extension (specs/language.md
# § Spine-based elaborator palette, specs/formal/elaborator.md § 12.4 —
# R-Widen-Sum).
#
# Pre-condition: μ(spine_sum(S)) ⊆ μ(spine_sum(T)) as multisets.
# The rule fires R-Widen-Sum: an `__left__` / `__right__` chain
# places the source value at its slot in the wider target, threaded
# through an `__either__` over the source. Value is preserved.
# R-Identity-Sum-intro is a special case: extending with `!` arms is
# admissible.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
