#!/bin/sh
set -u
cd workdir || exit 1
exec "$KIO_BIN" dep fetch
