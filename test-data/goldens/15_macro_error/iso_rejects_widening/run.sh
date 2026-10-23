#!/bin/sh
# `iso!` rejects sum widening — `A → A | B` extends the value-set
# (the target carries values the source can't supply, so the map
# isn't a bijection). Widening is `into!`-only per spec § iso! /
# into! / onto! mechanics. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
