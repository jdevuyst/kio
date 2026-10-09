#!/bin/sh
# A recognized-looking target name cannot choose its backend.
set -u
cd workdir || exit
"$KIO_BIN" build js --skip-unsupported-targets
