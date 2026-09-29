#!/bin/sh
# A scoped projector must keep its corresponding role name when retyped.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
