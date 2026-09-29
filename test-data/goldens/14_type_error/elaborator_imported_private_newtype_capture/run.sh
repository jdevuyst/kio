#!/bin/sh
# A consumer cannot make an imported private newtype part of a public
# elaborator merely by capturing one of the newtype's public members.
set -u

scratch=$(mktemp -d "${TMPDIR:?}/elaborator-private-capture.XXXXXX") || exit 1
trap 'rm -rf "$scratch"' 0 HUP INT TERM

cd workdir || exit 1
"$KIO_BIN" check >"$scratch/stdout" 2>"$scratch/stderr"
status=$?

cat "$scratch/stdout"
cat "$scratch/stderr" >&2

if [ "$status" -ne 14 ]; then
  printf 'elaborator_imported_private_newtype_capture: expected exit 14, got %s\n' "$status" >&2
  exit 1
fi
if ! grep -Fq 'p.Box' "$scratch/stderr"; then
  printf 'elaborator_imported_private_newtype_capture: diagnostic did not identify the private newtype\n' >&2
  exit 1
fi
# shellcheck disable=SC2016 # backticks are literal diagnostic text
if grep -Fq 'give captured member `p.Box.make` visibility' "$scratch/stderr"; then
  printf 'elaborator_imported_private_newtype_capture: diagnostic offered an edit to the provider\n' >&2
  exit 1
fi

exit 14
