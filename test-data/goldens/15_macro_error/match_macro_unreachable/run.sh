#!/bin/sh
# The zero-argument catch-all leaves no source branch for the later clause.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
