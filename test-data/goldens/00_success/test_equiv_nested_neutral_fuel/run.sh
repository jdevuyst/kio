#!/bin/sh
set -u
cd workdir || exit
"$KIO_BIN" check && "$KIO_BIN" test >/dev/null
