#!/bin/sh
# A parameter type checks the explicitly annotated literal carrier without replacing it.
set -eu
cd workdir
exec "$KIO_BIN" check
