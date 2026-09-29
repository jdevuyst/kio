#!/bin/sh
# An empty parameter list has one Unit value slot. The receiver fills that
# slot, so the trailing `()` over-applies the Unit result.
set -u
cd workdir || exit
"$KIO_BIN" check
