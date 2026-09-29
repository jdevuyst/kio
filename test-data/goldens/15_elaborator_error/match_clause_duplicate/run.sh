#!/bin/sh
set -eu
cd workdir
"$KIO_BIN" dep fetch >/dev/null
"$KIO_BIN" check
