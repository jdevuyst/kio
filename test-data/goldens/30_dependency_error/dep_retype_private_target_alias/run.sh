#!/bin/sh
# A retype target must export its selected type, not merely resolve a private
# binding to a public nominal declaration. Both selective and qualified
# dependency consumers retain that obligation. This is a frontend CLI case.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
