#!/bin/sh
# Macro selection and qualification obey their declared namespace.
set -eu
cd workdir
exec "$KIO_BIN" check
