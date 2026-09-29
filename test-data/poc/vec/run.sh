#!/bin/sh
# POC for `vec` -- a persistent indexed sequence backed by a fanout-2
# digit trie. The root package is the reusable library; `demo/` imports
# it as a dependency and supplies runner-facing host adapters.
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
  --package-name vec_demo \
  --protocol testapi-bare-collection \
  out/"$KIO_TARGET"
