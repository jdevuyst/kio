#!/bin/sh
# Escape permission is rejected when no ordinary closure boundary is crossed.
set -u
cd workdir || exit
"$KIO_BIN" check
