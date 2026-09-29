#!/bin/sh
# An unknown directive keyword in .md prose is a runner error (exit 70).
# The keyword `@eval` is not in the recognized vocabulary and must produce
# a diagnostic.
set -u
"$KIO_BIN" doc check
