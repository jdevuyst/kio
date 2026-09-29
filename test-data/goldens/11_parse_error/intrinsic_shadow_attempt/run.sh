#!/bin/sh
# A user attempts to shadow the `__pair__` intrinsic with a
# top-level `fn`. The parser's name validator rejects any
# declaration whose name begins with `__`, so the program never
# even reaches name resolution — exit 11 (parse error).
set -u
cd workdir || exit
"$KIO_BIN" check
