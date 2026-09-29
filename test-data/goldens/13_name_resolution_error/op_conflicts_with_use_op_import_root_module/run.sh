#!/bin/sh
# The root-module import and local declaration name distinct operator origins.
# The collision is reported after use validation as a name error (exit 13).
set -u
cd workdir || exit
"$KIO_BIN" check
