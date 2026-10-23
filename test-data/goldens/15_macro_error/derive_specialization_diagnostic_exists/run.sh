#!/bin/sh
# After one derivation succeeds, an underdetermined later candidate reports the
# specialization diagnostic from the existence-only ambiguity-counting path.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
