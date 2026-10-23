#!/bin/sh
# `onto!` rejects product duplication — `A → A & A` (the diagonal)
# adds a target slot the source has no second factor for. The map
# isn't a surjection of the *target* (the off-diagonal pairs `(a₁,
# a₂)` with a₁ ≠ a₂ have no source pre-image). Duplication is
# `into!`-only per spec § iso! / into! / onto! mechanics.
# Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
