#!/bin/sh
# A checked elaborator-error sentinel wins over the marked call's non-unit
# result equation.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
