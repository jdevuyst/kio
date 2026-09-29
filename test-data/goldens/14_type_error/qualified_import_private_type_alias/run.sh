#!/bin/sh
# A qualified module alias does not expose a private transparent type alias.
set -u
cd workdir || exit
"$KIO_BIN" check
