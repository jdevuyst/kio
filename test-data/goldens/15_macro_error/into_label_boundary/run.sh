#!/bin/sh
# The elaborator rejects coercions that would cross a label boundary:
# none of `iso!` / `into!` / `onto!` ever manufactures or strips a
# label wrapper. To move between a payload type and `F` you write
# `{f = x}` or call `F.get`. Exit code 15 (elaborator
# error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
