#!/bin/sh
# [`@source term`] directives in /// doc-comment prose resolve against
# the surrounding module's scope for all item kinds: fn, type, literal, labels,
# op. The [`@type term`] directive resolves against the value binding.
set -u
"$KIO_BIN" doc check
