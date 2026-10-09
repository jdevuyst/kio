#!/bin/sh
# Full Kio consumes this surface import; the receiving Prime boundary rejects it.
set -u
cd workdir || exit
"$KIO_BIN" check || exit
kio-prime check
