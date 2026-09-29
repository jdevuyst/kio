#!/bin/sh
# With no enclosing or explicit result and no independently closed clause
# result, `match!`'s symmetric common-result relation stays underdetermined.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
