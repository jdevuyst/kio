#!/bin/sh
# The elaborator rejects when no coercion exists from source type
# `(A & B)` to target `(A & C)` — `B` and `C` are distinct
# newtypes, and the elaborator's structural rules don't permit
# arbitrary type substitution. Uses `into!`; the same rejection
# would fire under `iso!` and `onto!`.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
