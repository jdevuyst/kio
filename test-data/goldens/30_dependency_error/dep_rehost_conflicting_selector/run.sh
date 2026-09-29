#!/bin/sh
# Duplicate rehost selectors fail in the shared dependency command.
set -u
cd workdir || exit 1
exec "$KIO_BIN" dep fetch
