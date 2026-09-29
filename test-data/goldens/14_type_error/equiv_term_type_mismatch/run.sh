#!/bin/sh
# `equiv` term bodies must share a type. When two terms synthesize
# distinct types — here `()` vs `Wrapped` — `kio test` fails with
# the type-error category (exit 14), not the test-failure category
# (exit 50). The runner reports nothing past the type-check step.
set -u
cd workdir || exit
"$KIO_BIN" test
