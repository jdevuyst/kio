#!/bin/sh
# [`@signature term`], [`@source term`], and [`@type term`] directives
# in /// doc-comment prose resolve against the surrounding module's
# scope. `@signature` covers `newtype`; `@type` resolves against a
# value binding. Valid directives must not produce any error.
set -u
"$KIO_BIN" doc check
