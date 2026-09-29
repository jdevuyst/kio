#!/bin/sh
# Proof-of-concept higher-kinded types: two arity-1 newtypes
# (`Box`, `Identity`) and the full HKT vocabulary exercised
# against both — brand intrinsics, abstract `[F]` binders
# flowing through generic code, a first-class polymorphic monad
# dictionary at a newtype field, and `do`-block syntax over a
# projected dictionary's `bind`. The brand-generic `rebrand` and
# `pipeline` run against either brand without source changes.
# See `docs/guides/higher-kinded-types.md` for the
# matching guide.
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
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" --protocol testapi-text-elab-int out/"$KIO_TARGET"
