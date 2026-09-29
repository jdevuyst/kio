#!/bin/sh
# A visibility fact is useful context, but it is not itself a concrete fix.
set -u

scratch=$(mktemp -d "${TMPDIR:?}/private-member-fact.XXXXXX") || exit 1
trap 'rm -rf "$scratch"' 0 HUP INT TERM

cd workdir || exit 1
"$KIO_BIN" check >"$scratch/stdout" 2>"$scratch/stderr"
status=$?

cat "$scratch/stdout"
cat "$scratch/stderr" >&2

if [ "$status" -ne 14 ]; then
  printf 'private_newtype_member_fact_not_help: expected exit 14, got %s\n' "$status" >&2
  exit 1
fi
if ! grep -Fq 'p.Box.seal' "$scratch/stderr"; then
  printf 'private_newtype_member_fact_not_help: diagnostic omitted the inaccessible member\n' >&2
  exit 1
fi
# shellcheck disable=SC2016 # backticks are literal diagnostic text
if grep -Fq '= help: `p.Box.seal` is private' "$scratch/stderr"; then
  printf 'private_newtype_member_fact_not_help: explanatory context was presented as a fix\n' >&2
  exit 1
fi

exit 14
