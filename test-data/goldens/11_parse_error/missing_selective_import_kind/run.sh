#!/bin/sh
# Every ordinary selective import names its declaration kind.
set -u
cd workdir || exit
"$KIO_BIN" check
