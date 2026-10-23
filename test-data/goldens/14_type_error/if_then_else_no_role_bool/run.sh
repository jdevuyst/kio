#!/bin/sh
# A qualified module import does not select its Boolean type as a local default,
# so the conditional intrinsic has no selected Boolean slot.
set -u
cd workdir || exit
"$KIO_BIN" check
