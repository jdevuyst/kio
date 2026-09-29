#!/bin/sh
# A [`@signature term`] directive whose term is not in scope produces
# a runner error (exit 70), same format as a broken intra-doc ref.
set -u
"$KIO_BIN" doc check
