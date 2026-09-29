#!/bin/sh
# A computed input must be determined before a marked implementation executes.
set -u
cd workdir || exit
"$KIO_BIN" check
