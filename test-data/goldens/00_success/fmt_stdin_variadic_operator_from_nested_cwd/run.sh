#!/bin/sh
set -u
cd workdir/nested || exit
"$KIO_BIN" fmt --check - < input.txt
