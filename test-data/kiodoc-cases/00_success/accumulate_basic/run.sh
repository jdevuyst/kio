#!/bin/sh
# An accumulating harness concatenates every {@NAME} member into
# one aggregate program. A later member can see earlier members'
# declarations.
set -u
"$KIO_BIN" doc check
