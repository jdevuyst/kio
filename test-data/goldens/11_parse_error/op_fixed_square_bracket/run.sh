#!/bin/sh
# Square brackets belong to varop delimiters, not fixed operator patterns.
set -u
cd workdir || exit
"$KIO_BIN" check
