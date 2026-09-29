#!/bin/sh
# Generated Kio' type positions and reflected elaborator inputs retain nominal
# identity when caller aliases and local type binders reuse their spellings.
set -u
cd workdir || exit
"$KIO_BIN" test >/dev/null || exit
"$KIO_BIN" check || exit
"$KIO_BIN" build kio-prime >/dev/null || exit
kio_prime="$(dirname "$KIO_BIN")/kio-prime"
cd out/kio-prime || exit
"$kio_prime" check
