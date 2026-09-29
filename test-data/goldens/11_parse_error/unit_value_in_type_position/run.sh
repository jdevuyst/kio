#!/bin/sh
# Unit value parentheses are not the unit type spelling.
set -u
cd workdir || exit
"$KIO_BIN" check
