#!/bin/sh
# An elaborator call with no corresponding ordinary import
# import is the standard unresolved-name error (exit 13). The
# diagnostic points the user at the `use` line to add.
set -u
cd workdir || exit
"$KIO_BIN" check
