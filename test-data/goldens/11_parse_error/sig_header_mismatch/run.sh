#!/bin/sh
# Reconciliation #3 — package-name coherence for `*.sig.kio`. The
# changelog header `signature <pkg> v(N);` must name the package
# (`app`), mirroring module-name coherence and the package-file stem
# check. Here the header says `wrong`, so `kio sig` rejects the file at
# the parse tier (exit 11) before any contract comparison runs.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module sources are Kio'-shaped) plus
# SKIP_KIO_PRIME_RUN to opt out of the kio-prime run.
set -u
cd workdir || exit 1
"$KIO_BIN" sig status
