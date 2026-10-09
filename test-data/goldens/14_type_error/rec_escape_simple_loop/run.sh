#!/bin/sh
# An escaping call requires the general provider; the error owns the selected path and source call.
set -u
cd workdir || exit
"$KIO_BIN" check
