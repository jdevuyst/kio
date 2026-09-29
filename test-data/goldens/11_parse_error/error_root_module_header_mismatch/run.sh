#!/bin/sh
# A module header must match the file's filesystem location (the path is
# the file's location relative to the package root, `.kio` stripped). A
# mismatch is a parse error (exit 11).
set -u
cd workdir || exit
"$KIO_BIN" check
