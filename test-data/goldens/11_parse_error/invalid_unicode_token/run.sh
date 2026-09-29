#!/bin/sh
# Formatting rejects a non-ASCII identifier character without changing the identifier grammar.
set -u
"$KIO_BIN" fmt - < workdir/main.kio
