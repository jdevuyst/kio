#!/bin/sh
# Numeric values are rejected by Kio checking before any host compilation.
set -eu
cd workdir
exec "$KIO_BIN" check
