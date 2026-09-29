#!/bin/sh
# An explicit `match!` result checks clause bodies but does not determine an
# omitted dispatch-pattern parameter type.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
