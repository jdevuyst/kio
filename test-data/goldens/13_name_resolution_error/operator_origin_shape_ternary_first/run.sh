#!/bin/sh
# The first imported shape parses deterministically; origin validation then
# rejects the distinct provider rather than silently choosing it.
set -u
cd workdir || exit
"$KIO_BIN" check
