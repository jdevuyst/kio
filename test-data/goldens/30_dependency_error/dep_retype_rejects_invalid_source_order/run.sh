#!/bin/sh
# Retyping cannot erase an invalid forward reference in the source module.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
