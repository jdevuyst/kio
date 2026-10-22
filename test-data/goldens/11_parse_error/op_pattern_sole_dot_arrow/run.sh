#!/bin/sh
# UFCS is native structure, not a fixed operator word.
set -u
cd workdir || exit
"$KIO_BIN" check
