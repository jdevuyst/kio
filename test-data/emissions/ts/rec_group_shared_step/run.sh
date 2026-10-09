#!/bin/sh
# SUBJECT: Recursive-group effect bodies stay single-copy in emitted TS runtime source.
# CONTRACT: Measured recursive-dispatcher body sharing.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-rec-group.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

find "out/$KIO_TARGET" -type f -name '*.js' -exec cat {} + > "$scratch/emitted"
for payload in shared-first-effect shared-second-before shared-second-after shared-third-effect shared-fourth-effect; do
  count=$(awk -v payload="$payload" '
    {
      rest = $0
      while ((at = index(rest, payload)) != 0) {
        count++
        rest = substr(rest, at + length(payload))
      }
    }
    END { print count + 0 }
  ' "$scratch/emitted")
  if [ "$count" -ne 1 ]; then
    printf '%s has %s emitted copies; the measured shared-body count is 1\n' "$payload" "$count" >&2
    exit 1
  fi
done
