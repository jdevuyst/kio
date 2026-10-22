#!/bin/sh
# Distinct contiguous operator words may share a character prefix.
set -u
cd workdir || exit
"$KIO_BIN" check
