#!/bin/sh
# The pattern's slot types already define the product type;
# pinning an outer `: T` is redundant and rejected by the
# parser. The fix is the as-pattern form `name: (...)` (binds
# whole + destructures) or the bare pattern (destructures
# only). See specs/language.md § Parameter patterns.
set -u
cd workdir || exit
"$KIO_BIN" check
