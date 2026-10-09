#!/bin/sh
# SUBJECT: Repeated generic callback boundaries keep total emitted source and growth bounded.
# CONTRACT: The 4/32-export witness measured 67073/267318 source/stub bytes without runtime value-plan tables.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-value-plan-size.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp large/api.kio "$scratch/large.kio"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
small=$(find "out/$KIO_TARGET" -type f \( -name '*.py' -o -name '*.pyi' \) -exec wc -c {} + | awk '$NF != "total" { bytes += $1 } END { print bytes + 0 }')
cp "$scratch/large.kio" api.kio
"$KIO_BIN" build "$KIO_TARGET"
large=$(find "out/$KIO_TARGET" -type f \( -name '*.py' -o -name '*.pyi' \) -exec wc -c {} + | awk '$NF != "total" { bytes += $1 } END { print bytes + 0 }')
if [ "$small" -le 0 ] || [ "$large" -le "$small" ]; then
  printf 'invalid small/large emitted sizes: %s/%s bytes\n' "$small" "$large" >&2
  exit 1
fi
if [ "$large" -gt 700000 ] || [ "$((large - small))" -gt 480000 ]; then
  printf 'repeated callback metadata exceeds measured size bounds: small=%s large=%s bytes\n' "$small" "$large" >&2
  exit 1
fi
