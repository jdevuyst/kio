#!/bin/sh
# Non-exhaustive `match!` reports an uncovered source arm through the
# imported elaborator.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
