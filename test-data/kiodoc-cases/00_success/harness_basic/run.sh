#!/bin/sh
# Harness-wrapped snippet: a `kio {harness=main}` fence declares
# the package skeleton, and a later `kio {@main}` snippet has its
# body substituted into the harness at `__INSERT_CODE_HERE__`.
set -u
"$KIO_BIN" doc check
