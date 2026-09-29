#!/bin/sh
# `ease!` rejects product duplication — `A → A & A` (the diagonal)
# is `into!`-only per spec § iso! / into! / onto! / align! / ease!
# / atom! mechanics. Excluding `R-Diag-Prod` is what defines `ease!`'s
# semantic identity: no source factor is ever used twice. Exit 15
# (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
