#!/bin/sh
# A `bridge { … }` glob naming a module path that matches no module
# in the package (`pkg/does_not_exist`) is a dead-glob bridge error
# (exit 20) — almost always a typo.
set -u
cd workdir || exit
"$KIO_BIN" check
