#!/bin/sh
set -eu
trap 'rm -rf out/docs out/docs-md' EXIT

"$KIO_BIN" doc check
"$KIO_BIN" doc build --md --html
node check.cjs
