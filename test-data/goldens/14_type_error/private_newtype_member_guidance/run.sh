#!/bin/sh
# A typo near a private newtype projector must not expose that projector as
# guidance to an outside caller; only the accessible constructor may appear.
set -u

scratch=$(mktemp -d "${TMPDIR:?}/private-member-guidance.XXXXXX") || exit 1
trap 'rm -rf "$scratch"' 0 HUP INT TERM

cd workdir || exit 1
"$KIO_BIN" check >"$scratch/stdout" 2>"$scratch/stderr"
status=$?

cat "$scratch/stdout"
cat "$scratch/stderr" >&2

if [ "$status" -ne 14 ]; then
  printf 'private_newtype_member_guidance: expected exit 14, got %s\n' "$status" >&2
  exit 1
fi
if grep -Fq 'seal' "$scratch/stderr"; then
  # shellcheck disable=SC2016 # backticks are literal diagnostic text
  printf 'private_newtype_member_guidance: diagnostic exposed private member `seal`\n' >&2
  exit 1
fi
if ! grep -Fq 'make' "$scratch/stderr"; then
  # shellcheck disable=SC2016 # backticks are literal diagnostic text
  printf 'private_newtype_member_guidance: diagnostic omitted accessible member `make`\n' >&2
  exit 1
fi

exit 14
