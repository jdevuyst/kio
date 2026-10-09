#!/bin/sh
set -u
cd workdir || exit

# Finite nested invocations pass on both cold and warm equiv-cache paths.
"$KIO_BIN" test >/dev/null 2>&1 || exit
"$KIO_BIN" test
