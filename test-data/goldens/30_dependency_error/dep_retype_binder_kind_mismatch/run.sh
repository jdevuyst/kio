#!/bin/sh
# A retype counterpart must preserve the effective kind of every binder.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
