#!/bin/sh
# [`name`] resolves names brought in by `use` statements, including
# qualified aliases (`use m/mod as alias`) and selectively-imported
# names.
set -u
"$KIO_BIN" doc check
