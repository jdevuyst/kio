#!/bin/sh
# The elaborator rejects coercions that cross a nominal boundary:
# none of `iso!` / `into!` / `onto!` manufactures or strips a
# nominal wrapper. Use `F.mk(x)` or `F.get` to cross it explicitly.
# Exit code 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
