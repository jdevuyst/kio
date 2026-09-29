#!/bin/sh
# The receiver fills the only value slot, so the remaining explicit
# type argument is checked like an ordinary extra argument.
set -u
cd workdir || exit
"$KIO_BIN" check
