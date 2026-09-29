#!/bin/sh
# A target projector restricted to a smaller subtree loses scoped member access.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
