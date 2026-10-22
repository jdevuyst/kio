#!/bin/sh
# A fixed operator component is one contiguous symbol word.
set -u
cd workdir || exit
"$KIO_BIN" check
