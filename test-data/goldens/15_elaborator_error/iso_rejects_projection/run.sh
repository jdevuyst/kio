#!/bin/sh
# `iso!` rejects product projection — `A & B → A` drops the `B`
# component, so the map isn't information-preserving. Projection
# is `onto!`-only per spec § iso! / into! / onto! mechanics.
# Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
