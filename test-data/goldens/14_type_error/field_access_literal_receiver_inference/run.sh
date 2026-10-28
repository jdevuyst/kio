#!/bin/sh
# This case checks the shared frontend inference boundary.
set -eu
cd workdir
exec "$KIO_BIN" check
