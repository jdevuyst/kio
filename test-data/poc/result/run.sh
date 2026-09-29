#!/bin/sh
# POC for `Result` — `Result(T, E)` as the bare structural sum `T | E`
# (success left, error right), with the named API surface an FP user
# expects: `ok` / `err` constructors, a `fold` eliminator built from a
# plain `match!`, functor / bifunctor (`map` / `map_err` / `bimap`),
# monad-in-success (`bind` / `apply`), recovery (`or_else` /
# `unwrap_or` / `unwrap_err_or`), the `swap` bridge, and a four-operator
# DSL (`?>` bind, `%>` map, `??` unwrap-or, `<*>` apply). The pure core
# lives in a `*.kio` file (pure, no `__intrinsics__`);
# the nested demo package renders the values and runs the tour.
#
# Chains `kio check` + `kio test` + per-backend build + run.
# `expected.stdout` snapshots `main`'s output only; `kio check` is
# silent on success and `kio test`'s status lines are discarded so the
# snapshot stays focused on the program's own output.
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
  --package-name result_demo \
  --protocol testapi-compute-root \
  out/"$KIO_TARGET"
