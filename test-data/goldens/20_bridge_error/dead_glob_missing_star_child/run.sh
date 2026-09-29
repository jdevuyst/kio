#!/bin/sh
# Dead-glob bridge check: `missing/*` names no module in the package.
set -u
cd workdir || exit
"$KIO_BIN" check
