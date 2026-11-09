#!/bin/sh
# Regression: an independently completed opaque input must close before fills materialization.
set -u
cd workdir || exit
"$KIO_BIN" check
