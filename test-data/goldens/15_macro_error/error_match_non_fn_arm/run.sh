#!/bin/sh
# A `match!` arms-tuple element that isn't of function type gives the
# imported elaborator no callable clause for that slot.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
