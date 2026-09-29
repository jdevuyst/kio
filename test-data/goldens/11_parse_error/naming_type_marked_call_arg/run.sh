#!/bin/sh
# A type-shaped raw call argument must satisfy the complete type-name grammar;
# the marker does not make a later uppercase letter valid.
set -u
cd workdir || exit
"$KIO_BIN" check
