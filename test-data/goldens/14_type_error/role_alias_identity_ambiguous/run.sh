#!/bin/sh
# Same-leaf host types from distinct qualified origins remain distinct after
# transparent rebinding; a conditional must report ambiguity deterministically.
set -u
cd workdir || exit
"$KIO_BIN" check
