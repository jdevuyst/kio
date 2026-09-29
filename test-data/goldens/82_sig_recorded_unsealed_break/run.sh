#!/bin/sh
# `kio sig status` exit 82 (a recorded break, not yet sealed), in its own
# exit-category bucket so the harness asserts the 82 verdict
# independently — not buried inside `sig_flow`'s `expect_exit` chain.
#
# The checked-in `app.sig.kio` seals v(1) recording the export
# `api.serve`, then the open v(2) draft records the break: `serve`
# removed (breaking) and `other` added (compatible). The live source
# matches that draft, so the break IS recorded but not yet sealed.
# `kio sig status` reports 82 and the case exits 82 directly (the bucket
# name pins the category).
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
