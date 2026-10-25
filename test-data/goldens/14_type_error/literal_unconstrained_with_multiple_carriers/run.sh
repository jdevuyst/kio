#!/bin/sh
# Importing multiple integer carriers does not determine an unannotated let value.
set -u
cd workdir || exit
"$KIO_BIN" check
