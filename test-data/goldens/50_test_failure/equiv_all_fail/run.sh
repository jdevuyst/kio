#!/bin/sh
# Two equiv blocks, both failing. Pins the runner's plural in the
# summary line ("2/2 equiv blocks failed") and the source-order
# emission of the per-block fail reports. Exits 50.
set -u
cd workdir || exit
"$KIO_BIN" test
