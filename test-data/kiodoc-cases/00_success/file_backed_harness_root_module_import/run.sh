#!/bin/sh
# Document-scoped `{file}` fences are written into the scratch package
# alongside a file-backed harness before `kio check` runs.
set -u
"$KIO_BIN" doc check
