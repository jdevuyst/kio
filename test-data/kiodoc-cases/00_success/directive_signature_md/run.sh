#!/bin/sh
# [`@signature term`], [`@source term`], and [`@type term`] directives
# in .md prose resolve against the package's export scope when an
# package file exists.
set -u
"$KIO_BIN" doc check
