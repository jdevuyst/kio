#!/bin/sh
# Dead-glob bridge check: a `bridge { … }` glob that matches no module
# in the package is rejected (exit 20, bridge error) — almost always a
# typo. Here `nope;` matches nothing.
set -u
cd workdir || exit
"$KIO_BIN" check
