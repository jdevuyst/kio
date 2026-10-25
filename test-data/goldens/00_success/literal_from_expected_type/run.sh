#!/bin/sh
# The same literal spelling follows independently known parameter and return types.
set -eu
cd workdir
exec "$KIO_BIN" check
