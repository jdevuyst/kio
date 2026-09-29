#!/bin/sh
# An elaborator error in a non-first declaration: `bad`'s `widen_sum!`
# widens an `A` into `(B | C)`, but `A` is no arm of that sum, so the
# elaborator rejects it. The stderr-grep checks the diagnostic message.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
