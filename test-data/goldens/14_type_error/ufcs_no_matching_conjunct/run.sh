#!/bin/sh
# UFCS dispatch fails when no conjunct of the receiver's type
# unifies with the callee's first parameter — exit 14 (type
# error).
set -u
cd workdir || exit
"$KIO_BIN" check
