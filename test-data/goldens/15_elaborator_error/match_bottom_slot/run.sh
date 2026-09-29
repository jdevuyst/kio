#!/bin/sh
# A `!`-typed clause has empty coverage and is unreachable.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
