#!/bin/sh
# `into!` rejects sum collapse — `A | A → A` identifies the two
# source branches, dropping the information about which branch
# the source landed in. `into!` is information-preserving; sum
# collapse is `onto!`-only per spec § iso! / into! / onto!
# mechanics. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
