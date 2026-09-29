#!/bin/sh
# A polymorphic clause covers `Box(Foo)` by binding `[A]` from the
# scrutinee branch, but it cannot cover the unrelated `Bar` branch, so
# the imported elaborator reports an uncovered source arm.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
