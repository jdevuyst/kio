#!/bin/sh
# A `literal` declaration substitutes bare references with the bound
# literal token and drops the declaration, so each use site types the
# literal against its surrounding context.
set -u
cd workdir || exit
"$KIO_BIN" check
