#!/bin/sh
# Round-trip pipeline: build to Kio' source, then re-build that
# Kio' source to the impl's primary target, then run it through
# the impl's runner. Asserts that the kinded HKT surface -- the
# applied brand `Wrap(a)` and the brand-crossing members
# `Wrap.mk_wrap` / `Wrap.un_wrap` -- survives the Kio' boundary intact.
set -u
cd workdir || exit
"$KIO_BIN" build kio-prime || exit
cd out/kio-prime || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" --package-name exec_app_inj_prj_roundtrip_prime \
  --protocol testapi-print out/"$KIO_TARGET"
