#!/bin/sh
# Same-leaf host types from distinct qualified origins remain distinct after
# transparent rebinding; specializing a conditional to the second identity
# rejects a condition carrying the first identity.
set -u
cd workdir || exit
"$KIO_BIN" check
