#!/bin/sh
# Retyping cannot erase a source declaration's required recursive marker.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
