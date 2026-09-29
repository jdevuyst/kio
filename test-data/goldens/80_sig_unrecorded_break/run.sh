#!/bin/sh
# `kio sig status` exit 80 (breaks the last sealed contract, unrecorded),
# in its own exit-category bucket so the harness asserts the 80 verdict
# independently — not buried inside `sig_flow`'s `expect_exit` chain.
#
# The checked-in `app.sig.kio` seals v(1) recording the export
# `api.serve`. The live source dropped `serve` (it now exports `other`),
# and the open v(2) draft does NOT record that removal — an unrecorded
# break against the sealed contract. `kio sig status` reports 80 and the
# case exits 80 directly (the bucket name pins the category).
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
