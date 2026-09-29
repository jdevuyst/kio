#!/bin/sh
# POC for `dict` -- an ordered key-value map backed by a red-black
# balanced tree. The root package is the reusable comparator-explicit
# library; `demo/` imports it as a dependency and supplies runner-facing
# host adapters plus I32 demo operators.
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
  --package-name dict_demo \
  --protocol testapi-bare-collection \
  out/"$KIO_TARGET"
