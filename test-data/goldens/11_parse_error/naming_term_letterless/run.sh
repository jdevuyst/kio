#!/bin/sh
# A name must contain at least one letter: role classification keys on
# the case of the first letter, so the letterless value spellings
# (`_`, `_1`, `_1_2`, …) name nothing. `_` alone is the wildcard
# binder. This pins the rule at a term-declaration name site.
set -u
cd workdir || exit
"$KIO_BIN" check
