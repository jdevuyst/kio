#!/bin/sh
# Completed braced peers inside a build block need a semicolon between them.
set -u
cd workdir || exit
"$KIO_BIN" check
