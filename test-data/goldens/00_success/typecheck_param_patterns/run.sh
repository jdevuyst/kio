#!/bin/sh
# Exercises bare destructuring, nested destructuring, as-patterns,
# wildcard slots at every level, and let-destructuring in param and
# body positions. The build block routes through the
# kio-prime-roundtrip per-case check, which builds the package to
# Kio' and re-builds every applicable backend; multiple wildcard
# slots in one binder list must remain distinct generated parameters.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
