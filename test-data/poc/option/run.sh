#!/bin/sh
# POC for `option` — an `Option(A)` library built on Kio's native sums
# via the `labels`-shape `present(A) | .`, with constructors,
# eliminators, functor / applicative / monad, combinators, a bridge to
# bare `A | .` sums, host-backed fold-driven collection helpers, and a
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
  --package-name option_demo \
  --protocol testapi-bare-collection \
  out/"$KIO_TARGET"
