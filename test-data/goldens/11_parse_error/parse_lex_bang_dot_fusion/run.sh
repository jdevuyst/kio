#!/bin/sh
# `x.iso!.atom!` exercises the lexer's greedy `!.` fusion on the
# way into the generic bang-call parser. The source supplies `.atom!`
# where a call argument list must start, so the parser reports the
# missing `(` and exits 11.
set -u
cd workdir || exit
"$KIO_BIN" check
