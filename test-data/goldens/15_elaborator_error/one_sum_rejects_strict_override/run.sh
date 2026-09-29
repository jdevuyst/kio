#!/bin/sh
# `one_sum!` narrows a sum to a single surviving arm. The call site
# supplies an explicit target `B`, but the source contains only `A` arms,
# so the imported elaborator rejects the target. Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
