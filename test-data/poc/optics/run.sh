#!/bin/sh
# POC for optics — lens, prism, iso — as plain function pairs, with
# the usual combinators (view/set/over/compose for lenses;
# preview/review/over/compose for prisms; view_iso/review_iso/over/
# compose for isos) and coercions iso → lens / iso → prism.
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
  --package-name optics_demo \
  --protocol testapi-bare-arith-bool-i32 \
  out/"$KIO_TARGET"
