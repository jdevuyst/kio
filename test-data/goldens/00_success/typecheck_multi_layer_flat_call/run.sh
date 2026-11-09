#!/bin/sh
# Checks application through two successive type and value stages.
set -u
cd workdir || exit
"$KIO_BIN" check
