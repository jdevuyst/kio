#!/bin/sh
# A `pub(P)`-scoped newtype constructor whose `P` is not a prefix of the
# declaring module is rejected at name resolution (exit 12) — the same
# prefix rule the declaration's own `pub(P)` obeys, now enforced on the
# constructor/projector members too.
set -u
cd workdir || exit
"$KIO_BIN" check
