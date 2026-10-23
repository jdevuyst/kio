#!/bin/sh
# A macro call with no corresponding import is the standard
# unresolved-name error (exit 13), identifying the missing binding.
set -u
cd workdir || exit
"$KIO_BIN" check
