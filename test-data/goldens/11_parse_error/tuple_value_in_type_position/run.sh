#!/bin/sh
# Tuple value commas are not product type separators.
set -u
cd workdir || exit
"$KIO_BIN" check
