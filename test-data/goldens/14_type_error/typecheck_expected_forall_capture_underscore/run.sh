#!/bin/sh
# The inferred inner result cannot inhabit the outer result type.
set -u
cd workdir || exit
"$KIO_BIN" check
