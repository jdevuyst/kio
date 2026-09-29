#!/bin/sh
# Unterminated string literal — lexer rejects, exit 11.
set -u
cd workdir || exit
"$KIO_BIN" check
