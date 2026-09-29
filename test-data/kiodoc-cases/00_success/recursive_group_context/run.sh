#!/bin/sh
# The selected member retains the complete recursive declaration as its source.
set -eu

trap 'rm -rf out/docs-md' EXIT

"$KIO_BIN" doc build --md
sed -n '/<h3 id="item-pkg-Node"/,$p' out/docs-md/pkg.md | sed 's/<[^>]*>//g; /^[[:space:]]*$/d'
