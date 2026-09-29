#!/bin/sh
# Repeated `derive!` sites over a nested recursive target with a wider
# candidate tuple. Normal CI treats this as correctness coverage; manual
# debug runs make rule-set reuse and candidate-scan counts visible on a
# shape that is closer to "users got fancy".
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check
