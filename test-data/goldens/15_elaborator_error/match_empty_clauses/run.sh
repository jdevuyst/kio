#!/bin/sh
# A `match!` call with no clauses gives the imported elaborator no
# function clause to dispatch to, so elaboration fails.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
