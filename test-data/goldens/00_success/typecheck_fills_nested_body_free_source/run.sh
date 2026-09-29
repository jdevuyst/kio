#!/bin/sh
set -u
cd workdir || exit
# The subject is shared source typing, not a generated backend artifact.
"$KIO_BIN" check && "$KIO_BIN" test >/dev/null
