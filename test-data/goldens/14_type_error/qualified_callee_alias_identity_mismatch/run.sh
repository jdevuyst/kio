#!/bin/sh
# Caller module aliases cannot reinterpret either an already-qualified callee
# head or the children of its compound alias body, including through a
# let-bound callee value.
set -u
cd workdir || exit
"$KIO_BIN" check
