#!/bin/sh
# Retyping one member leaves its compact nominal peer nominally distinct.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
