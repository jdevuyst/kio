#!/bin/sh
# Bare and paired headers share one exact target-name namespace.
set -u
cd workdir || exit
"$KIO_BIN" build
