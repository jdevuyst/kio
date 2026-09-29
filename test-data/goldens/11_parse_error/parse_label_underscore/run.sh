#!/bin/sh
# Labels declared via `labels` must match [a-z][a-z0-9_]* — the
# leading underscore is rejected because the generated newtype
# name (the label's spelling with first letter capitalized) has no
# clean capitalization rule for `_`-prefixed spellings. Exit 11
# (parse error).
set -u
cd workdir || exit
"$KIO_BIN" check
