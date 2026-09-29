#!/bin/sh
# A `bridge { … }` glob naming an unknown module path (`nope/box`) is
# a dead-glob bridge error (exit 20).
set -u
cd workdir || exit
"$KIO_BIN" check
