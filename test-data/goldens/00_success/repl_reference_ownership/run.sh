#!/bin/sh
# Source-only declarations keep their provider and namespace at every use.
set -eu
cd workdir || exit
"$KIO_BIN" test >/dev/null
printf '%s\n' \
    ':refs truth' \
    ':refs second.truth' \
    ':refs first.proof' \
    ':refs build' \
    ':refs Field' \
    ':refs second.Field' \
    ':refs field' \
    ':refs proof' \
    ':refs first.field' \
    ':which proof' \
    ':quit' | "$KIO_BIN" repl demo/main
