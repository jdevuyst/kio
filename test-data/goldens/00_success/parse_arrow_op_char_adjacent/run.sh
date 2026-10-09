#!/bin/sh
# Check fused `->!` in a type expression and `->&` / `->|` in a
# signature's own return, alongside grouped direct chain results.
set -u
cd workdir || exit
"$KIO_BIN" check
