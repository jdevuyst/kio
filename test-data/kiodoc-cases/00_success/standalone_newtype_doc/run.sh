#!/bin/sh
# Both snippet validation and published declaration prose belong to the newtype.
set -eu
trap 'rm -rf out/docs-md' EXIT

"$KIO_BIN" doc build --md
grep -Fq 'Validated ordinary newtype docs.' out/docs-md/pkg.md
grep -Fq 'fn identity' out/docs-md/pkg.md
sed 's/<[^>]*>//g' out/docs-md/pkg.md | grep -Fq 'newtype Wrapped'
printf '%s\n' 'ordinary newtype documentation validated and rendered'
