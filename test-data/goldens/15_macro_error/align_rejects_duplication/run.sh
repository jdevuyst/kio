#!/bin/sh
# `align!` rejects product duplication — `A → A & A` (the
# diagonal) is `into!`-only per spec § iso! / into! / onto! /
# align! mechanics. align! omits dup deliberately (the diagonal
# is rare and creates fake structure the type system can't track
# as a constraint). Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
