#!/bin/sh
# POC for `Queue` -- an amortized-O(1) purely-functional FIFO queue in
# Okasaki's two-list representation. The root package is the reusable
# library; `demo/` imports it as a dependency and supplies runner-facing
# host adapters.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch --force >/dev/null || exit
"$KIO_BIN" check >/dev/null || exit
"$KIO_BIN" test >/dev/null || exit
cd demo || exit
"$KIO_BIN" dep fetch --force >/dev/null || exit
"$KIO_BIN" check >/dev/null || exit
"$KIO_BIN" test >/dev/null || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" \
  --package-name queue_demo \
  --protocol testapi-compute-root \
  out/"$KIO_TARGET"
