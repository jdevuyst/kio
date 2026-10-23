#!/bin/sh
# The `String` and Unit clause results cannot inhabit one common match result,
# so the symmetric result relation reports an ordinary type mismatch.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
