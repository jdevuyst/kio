#!/bin/sh
# A candidate whose unused leading binder cannot be specialized reports the
# diagnostic from the term-producing rule path instead of treating it as a miss.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
