#!/bin/sh
# SUBJECT: The TypeScript declaration skin exposes exact flat labels products and three-arm sums to strict hosts.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM

cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

(
  cd "$scratch/host"
  tsc --strict --noEmit --pretty false consumer.ts

  if negative_output=$(tsc --strict --noEmit --pretty false consumer_nonexhaustive.ts 2>&1); then
    printf 'consumer_nonexhaustive.ts unexpectedly type-checked\n' >&2
    exit 1
  fi

  negative_error_count=$(printf '%s\n' "$negative_output" | grep -c 'error TS' || true)
  if [ "$negative_error_count" -ne 1 ] ||
     ! printf '%s\n' "$negative_output" | grep -F 'consumer_nonexhaustive.ts' >/dev/null ||
     ! printf '%s\n' "$negative_output" | grep -F 'error TS2322:' >/dev/null ||
     ! printf '%s\n' "$negative_output" | grep -F 'Rejected' >/dev/null ||
     ! printf '%s\n' "$negative_output" | grep -F "not assignable to type 'never'" >/dev/null; then
    printf 'consumer_nonexhaustive.ts failed for an unintended reason:\n%s\n' "$negative_output" >&2
    exit 1
  fi
)
