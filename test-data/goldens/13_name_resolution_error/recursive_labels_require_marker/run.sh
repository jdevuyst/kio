#!/bin/sh
# A labels-generated nominal cycle needs one atomic `rec labels` scope.
set -u
cd workdir || exit
"$KIO_BIN" check
