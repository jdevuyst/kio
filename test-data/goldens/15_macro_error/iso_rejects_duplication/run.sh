#!/bin/sh
# `iso!` rejects product duplication — `A → A & A` (the diagonal)
# extends the source factor list, so the map isn't a bijection.
# Duplication is `into!`-only per spec § iso! / into! / onto!
# mechanics. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
