#!/bin/sh
# `kio sig status` exit 81 (stale-but-compatible drift), in its own
# exit-category bucket so the harness asserts the 81 verdict
# independently — not buried inside `sig_flow`'s `expect_exit` chain.
#
# A fresh package (`api.serve` exported, no `*.sig.kio` yet) has an
# unrecorded compatible addition against the empty sealed baseline: no
# host items, so no breaking env add. `kio sig status` reports 81 and
# the case exits 81 directly (the bucket name pins the category).
#
# `kio sig status` writes nothing, so the case runs in the shared
# `workdir/` without a scratch copy.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module source is Kio'-shaped) plus SKIP_KIO_PRIME_RUN
# to opt out of the kio-prime run.
set -u
cd workdir || exit 1
exec "$KIO_BIN" sig status
