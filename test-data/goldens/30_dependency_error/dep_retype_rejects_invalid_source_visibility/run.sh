#!/bin/sh
# Retyping cannot erase an invalid source signature visibility edge.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
