#!/bin/sh
# The shared Python support namespace cannot name a generated package.
set -u
cd workdir || exit
"$KIO_BIN" build
