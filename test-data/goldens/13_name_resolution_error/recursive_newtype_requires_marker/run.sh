#!/bin/sh
# A singleton nominal cycle must opt into its own head with `rec newtype`.
set -u
cd workdir || exit
"$KIO_BIN" check
