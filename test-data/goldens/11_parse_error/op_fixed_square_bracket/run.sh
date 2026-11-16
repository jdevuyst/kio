#!/bin/sh
# Square brackets belong to op_var delimiters, not fixed operator patterns.
set -u
cd workdir || exit
"$KIO_BIN" check
