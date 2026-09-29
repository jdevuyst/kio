#!/bin/sh
# Explicit type application keeps the fixture in the Kio' subset.
set -u
cd workdir || exit
"$KIO_BIN" check
