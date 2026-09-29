#!/bin/sh
# A module name is a value-role name, so it too must carry at least
# one letter. The declaration `module _1;` is letterless and rejected
# at parse (exit 11); `_1` lexes as an ordinary identifier, distinct
# from the wildcard slot token `_`.
set -u
cd workdir || exit
"$KIO_BIN" check
