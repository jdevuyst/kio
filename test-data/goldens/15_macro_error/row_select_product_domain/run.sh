#!/bin/sh
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
