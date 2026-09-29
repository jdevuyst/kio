#!/bin/sh
# Repeated `derive!` sites over the same recursive target. This is a
# correctness golden in normal CI, and a focused operation-count
# fixture for derive-memo measurement: the repeated sites exercise
# recursive resolution hits and make rule-set rebuilds visible without
# mixing in target codegen or runtime execution.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check
