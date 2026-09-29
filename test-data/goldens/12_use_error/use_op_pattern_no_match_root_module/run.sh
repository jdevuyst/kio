#!/bin/sh
# `import arith(op _ ??);` is an import error (exit 12) when the
# root module `arith` exports no matching public operator grammar.
set -u
cd workdir || exit
"$KIO_BIN" check
