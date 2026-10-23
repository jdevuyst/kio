#!/bin/sh
# Same-leaf host types from distinct qualified origins remain distinct after
# transparent rebinding; explicitly selecting both aliases conflicts eagerly.
set -u
cd workdir || exit
"$KIO_BIN" check
