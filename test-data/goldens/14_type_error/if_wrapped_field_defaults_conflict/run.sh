#!/bin/sh
# Branch-local field updates choose their own product order,
# including updates behind an ordinary let binding; the incompatible
# branch results must not be coerced into a common default.
set -u
cd workdir || exit
"$KIO_BIN" check
