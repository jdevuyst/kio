#!/bin/sh
# A package declaration is not a regular module header.
set -u
cd workdir || exit
"$KIO_BIN" check
