#!/bin/sh
# `use` of a non-`pub` identifier — exit 12.
set -u
cd workdir || exit
"$KIO_BIN" check
