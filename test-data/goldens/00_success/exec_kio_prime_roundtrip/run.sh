#!/bin/sh
# Round-trip pipeline: build to Kio' source, then re-build that
# Kio' source to the impl's primary target, then run it through
# the impl's runner. Verifies that the emitted Kio' is
# syntactically valid Kio' (the prime backend's contract) and
# behaviorally identical to the original (the desugaring and
# typer-time elaboration steps were total and behavior-preserving).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" build kio-prime || exit
cd out/kio-prime || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" --package-name exec_kio_prime_roundtrip \
  --protocol testapi-print-elab-bool-string out/"$KIO_TARGET"
