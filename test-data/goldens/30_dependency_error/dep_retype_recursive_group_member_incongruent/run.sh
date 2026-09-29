#!/bin/sh
# Retyping only one member of a mutual group leaves its peer nominally distinct.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
