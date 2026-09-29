#!/bin/sh
# An extra header name does not form a module path.
set -u
cd workdir || exit
"$KIO_BIN" check
