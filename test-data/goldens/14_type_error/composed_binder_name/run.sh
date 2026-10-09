#!/bin/sh
# A synthesized mismatch reports the written quantifier after alpha-renaming.
set -u
cd workdir || exit
"$KIO_BIN" check
