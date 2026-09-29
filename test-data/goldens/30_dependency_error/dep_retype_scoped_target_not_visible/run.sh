#!/bin/sh
# Source visibility is compared after dependency paths are re-rooted.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
