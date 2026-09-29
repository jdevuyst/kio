#!/bin/sh
# The tab-indented body must retain its source location in a type diagnostic.
set -u
cd workdir || exit
"$KIO_BIN" check
