#!/bin/sh
# A scoped source newtype must not become globally public after retyping.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
