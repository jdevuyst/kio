#!/bin/sh
# A label constructor nested beneath its generated field accessor must retain
# one receiver publication when its payload is checked in a later pass.
set -u
cd workdir || exit
"$KIO_BIN" check
