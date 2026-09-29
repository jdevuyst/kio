#!/bin/sh
set -u
cd workdir || exit
"$KIO_BIN" build "$KIO_TARGET" kio-prime || exit
cd out/kio-prime || exit
"$KIO_BIN" check
