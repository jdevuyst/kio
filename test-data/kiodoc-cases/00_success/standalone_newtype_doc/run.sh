#!/bin/sh
# Both snippet validation and published declaration prose belong to the nominal type.
set -eu
trap 'rm -rf out/docs-md' EXIT

"$KIO_BIN" doc build --md
grep -Fq 'Validated ordinary nominal type docs.' out/docs-md/pkg.md
grep -Fq 'fn identity' out/docs-md/pkg.md
sed 's/<[^>]*>//g' out/docs-md/pkg.md | grep -Fq 'type { Wrapped:'
printf '%s\n' 'ordinary nominal type documentation validated and rendered'
