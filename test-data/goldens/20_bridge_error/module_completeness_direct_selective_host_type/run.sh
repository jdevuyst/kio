#!/bin/sh
# Module-completeness bridge check: bridged `app` reaches host-bearing `cap` through a selective import.
set -u
cd workdir || exit
"$KIO_BIN" check
