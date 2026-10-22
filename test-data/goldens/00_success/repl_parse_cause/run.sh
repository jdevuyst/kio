#!/bin/sh
# Shared REPL parsing and source diagnostics; no generated backend is consumed.
set -eu
cd workdir || exit
"$KIO_BIN" test >/dev/null
printf '%s\n' \
    'index(x., 0)' \
    ':t index(x., 0)' \
    ':pure index(x., 0)' \
    ':normalize index(x., 0)' \
    ':t [* () *]' \
    ':normalize ()' \
    ':quit' | "$KIO_BIN" repl demo
