#!/bin/sh
# SUBJECT: Type declarations follow the same letter-then-digit word boundary.
set -u
cd workdir || exit
"$KIO_BIN" check
