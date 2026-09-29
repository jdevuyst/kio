#!/bin/sh
# A qualified module alias exposes only declarations visible from the
# importing module. Private implementation functions remain inaccessible.
set -u
cd workdir || exit
"$KIO_BIN" check
