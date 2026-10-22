#!/bin/sh
# Quoting a token does not let a fixed operator word contain a dot.
set -u
cd workdir || exit
"$KIO_BIN" check
