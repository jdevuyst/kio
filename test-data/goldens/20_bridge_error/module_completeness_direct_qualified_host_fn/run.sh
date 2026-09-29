#!/bin/sh
# Module-completeness bridge check: bridged `app` reaches host-bearing `cap` through a qualified import.
set -u
cd workdir || exit
"$KIO_BIN" check
