#!/bin/sh
# No fixed operator word may contain a dot, including dot-led components.
set -u
cd workdir || exit
"$KIO_BIN" check
