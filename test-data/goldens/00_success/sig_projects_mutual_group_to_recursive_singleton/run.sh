#!/bin/sh
set -u

base=$PWD
mkdir -p "$base/out/case-scratch" || exit 1
scratch=$(mktemp -d "$base/out/case-scratch/sig-rec-projection.XXXXXX") || exit 1
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

signature=$("$KIO_BIN" sig stage --force --stdout) || exit 1
# shellcheck disable=SC2016 # Backticks are literal diagnostic text.
for required in 'pub rec newtype A : A | B' 'pub newtype B : .'; do
  if ! printf '%s\n' "$signature" | grep -Fq "$required"; then
    printf 'projected signature omitted `%s`:\n%s\n' "$required" "$signature" >&2
    exit 1
  fi
done
if printf '%s\n' "$signature" | grep -Fq 'rec {'; then
  printf 'projected signature retained an invalid one-member recursive group:\n%s\n' "$signature" >&2
  exit 1
fi

# Reparse the exact fresh artifact and require replay to agree with the live
# contract. This independently checks the representation emitted above.
printf '%s\n' "$signature" > app.sig.kio
"$KIO_BIN" sig status >/dev/null
