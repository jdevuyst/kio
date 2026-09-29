#!/bin/sh
# POC for `list` — a `List(A)` linked-list library built on Kio's native
# self-referential sum `. | (A & List(A))`, with O(1) constructors /
# destructor / accessors, ordinary sum results for the partial
# accessors, host-backed loop-driven spine walks (`length` /
# `reverse` / `foldl` / `foldr` / `map` / `filter` / `append` / `take` /
# `drop` / `elem` / `find` / `at` / `index_of` / `to_string`), and a
# small operator DSL. The library package declares the host capabilities
# it needs directly; the nested demo package imports it as a dependency
# and rehosts those requirements to the runner's `testapi/*` modules.
#
# Chains `kio check` + `kio test` + per-backend build + run.
# `expected.stdout` snapshots `main`'s output only; `kio check`
# is silent on success and `kio test`'s status lines are
# discarded so the snapshot stays focused on the program's own
# output.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check >/dev/null || exit
"$KIO_BIN" test >/dev/null || exit
cd demo || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check >/dev/null || exit
"$KIO_BIN" test >/dev/null || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" \
  --package-name list_demo \
  --protocol testapi-arith-collection \
  out/"$KIO_TARGET"
