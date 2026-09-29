#!/bin/sh
# Formatting rejects an escape outside the admitted string escape set.
set -u
"$KIO_BIN" fmt - < workdir/main.kio
