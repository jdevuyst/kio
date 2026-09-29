#!/bin/sh
# A selective import names either an exact value or an exact type; `_FooBar`
# fits neither role because type-name suffixes contain no later uppercase.
set -u
cd workdir || exit
"$KIO_BIN" check
