#!/bin/sh
# Nested applications retain their consumed plans until the enclosing slot
# supplies an expected result type.
set -u
cd workdir || exit
"$KIO_BIN" check
