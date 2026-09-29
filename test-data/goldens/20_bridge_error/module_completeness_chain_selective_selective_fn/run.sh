#!/bin/sh
# Module-completeness bridge check: bridged `app` reaches host-bearing `cap` through `mid`.
set -u
cd workdir || exit
"$KIO_BIN" check
