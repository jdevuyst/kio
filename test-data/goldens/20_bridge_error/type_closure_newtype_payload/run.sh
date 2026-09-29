#!/bin/sh
# Type-closure bridge check: bridged `app` exposes `lib/types.Token` without bridging its module.
set -u
cd workdir || exit
"$KIO_BIN" check
