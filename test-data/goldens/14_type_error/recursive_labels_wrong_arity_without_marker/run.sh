#!/bin/sh
# The underlying arity defect takes precedence over a missing marker.
set -u
cd workdir || exit
"$KIO_BIN" check
