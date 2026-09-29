#!/bin/sh
# A package name is a value-role name and must carry at least one
# letter. The `.pkg.kio` directive `package _;` is letterless and
# rejected at parse (exit 11); `_` alone is the wildcard binder, not
# a name.
set -u
cd workdir || exit
"$KIO_BIN" check
