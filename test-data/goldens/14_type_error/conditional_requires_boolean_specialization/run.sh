#!/bin/sh
set -u
cd workdir || exit
exec "$KIO_BIN" check
