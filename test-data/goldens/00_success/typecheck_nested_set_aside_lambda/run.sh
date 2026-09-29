#!/bin/sh
# A nested call waits when its own set-aside lambda cannot pin a type argument.
set -u
cd workdir || exit
"$KIO_BIN" check
