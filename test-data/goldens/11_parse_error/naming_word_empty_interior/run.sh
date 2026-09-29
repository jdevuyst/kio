#!/bin/sh
# SUBJECT: Internal word separators do not form underscore runs.
set -u
cd workdir || exit
"$KIO_BIN" check
