#!/bin/sh
# A qualified Boolean carrier specializes the conditional without a local type import.
set -u
cd workdir || exit
"$KIO_BIN" check
