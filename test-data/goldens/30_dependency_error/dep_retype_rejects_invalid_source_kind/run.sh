#!/bin/sh
# Retyping cannot erase an ill-kinded source payload argument.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
