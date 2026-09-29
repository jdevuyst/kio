#!/bin/sh
# The imported shape parses deterministically; origin validation then rejects
# the local declaration rather than silently choosing either source.
set -u
cd workdir || exit
"$KIO_BIN" check
