#!/bin/sh
# [`@type term`] directives in /// doc-comment prose resolve against
# the surrounding module's scope. `@type` resolves against value
# bindings — a `fn`, and a `newtype`'s constructor / projector (which
# are value-level functions). Valid directives produce no error.
set -u
"$KIO_BIN" doc check
