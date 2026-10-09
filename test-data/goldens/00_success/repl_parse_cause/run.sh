#!/bin/sh
# Shared REPL parsing and source diagnostics; no generated backend is consumed.
set -eu
cd workdir || exit
"$KIO_BIN" test >/dev/null
printf '%s\n' \
    'index([1], 0)' \
    ':t index([1], 0)' \
    ':pure index([1], 0)' \
    ':normalize index([1], 0)' \
    ':t [* () *]' \
    ':normalize ()' \
    ':quit' | "$KIO_BIN" repl demo
