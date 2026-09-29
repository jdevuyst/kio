#!/bin/sh
# A [`@signature term`] directive in .md prose whose term is not in the
# export scope produces a runner error (exit 70).
set -u
"$KIO_BIN" doc check
