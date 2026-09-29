#!/bin/sh
# The argument requirement comes from the imported function's annotation,
# not the nominal type's definition or the caller's equally numbered bytes.
set -u
cd workdir || exit
"$KIO_BIN" check
