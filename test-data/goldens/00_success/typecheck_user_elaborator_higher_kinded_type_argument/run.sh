#!/bin/sh
# A value-shaped higher-kinded constructor in a named elaborator's type slot
# is reflected as a type and is not replayed as a runtime source expression.
set -u
cd workdir || exit
"$KIO_BIN" check
