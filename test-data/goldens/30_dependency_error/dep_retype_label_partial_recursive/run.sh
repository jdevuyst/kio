#!/bin/sh
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
