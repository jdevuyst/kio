#!/bin/sh
# Compatible public/scoped roles preserve selective and qualified callers;
# This is a frontend case.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch --force >/dev/null || exit
"$KIO_BIN" check >/dev/null || exit
printf 'member interfaces preserved\n'
