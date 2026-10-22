#!/bin/sh
# Fixed operator patterns require at least one value operand slot.
set -u
cd workdir || exit
"$KIO_BIN" check
