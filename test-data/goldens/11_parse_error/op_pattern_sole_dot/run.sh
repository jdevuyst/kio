#!/bin/sh
# A standalone dot is native structure, not a fixed operator word.
set -u
cd workdir || exit
"$KIO_BIN" check
