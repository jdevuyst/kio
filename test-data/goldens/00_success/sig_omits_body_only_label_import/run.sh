#!/bin/sh
set -u

base=$PWD
mkdir -p "$base/out/case-scratch" || exit 1
scratch=$(mktemp -d "$base/out/case-scratch/sig-label.XXXXXX") || exit 1
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio workdir/labels.kio workdir/types.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

output=$("$KIO_BIN" sig stage --force --stdout) || exit 1
if printf '%s\n' "$output" | grep -Fq '_label_'; then
  printf 'recorded signatures must not contain body-only generated module aliases\n' >&2
  printf '%s\n' "$output" >&2
  exit 1
fi
for required in 'import types(A);' 'import types(Kept);' 'import types as t;'; do
  if ! printf '%s\n' "$output" | grep -Fq "$required"; then
    printf 'recorded signatures dropped required import: %s\n' "$required" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
done
