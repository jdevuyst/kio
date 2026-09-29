#!/bin/sh
# `kio sig status` exit 80 on a FRESH package whose bridged module
# declares host requirements — the breaking-host-add-vs-empty-baseline
# path, the dual of `sig_flow`'s export-only 81 step.
#
# A fresh package has the empty sealed baseline. Adding an export is
# compatible (81), but adding a host requirement (`host type` / `host
# fn`) is BREAKING — env requirements are contravariant: an existing host
# is not guaranteed to supply a newly-demanded capability. Here `api`
# declares `host type H` + `host fn make` (plus the export `serve`), so
# against the empty baseline the contract has an unrecorded breaking host
# add. `kio sig status` reports 80 and the case exits 80 directly (the
# bucket name pins the category).
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
