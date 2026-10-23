#!/bin/sh
# `iso!` rejects sum collapse — `A | A → A` identifies the two
# source branches, so the map isn't a bijection (two source values
# share one target value). Sum collapse is `onto!`-only per spec
# § iso! / into! / onto! mechanics. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
