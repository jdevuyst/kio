#!/bin/sh
# Compact recursive type declarations require an explicit `rec` marker.
set -u
cd workdir || exit
"$KIO_BIN" check
