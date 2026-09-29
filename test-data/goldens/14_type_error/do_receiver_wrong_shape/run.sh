#!/bin/sh
# The one-argument `not_bind` receiver cannot accept the block's
# continuation: its Box result is applied like a function.
set -u
cd workdir || exit
"$KIO_BIN" check
