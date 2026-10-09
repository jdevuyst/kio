#!/bin/sh
# The nearest prose hint remains useful, but an unsafe replacement is absent.
# JS only routes this frontend diagnostic; no backend artifact is consumed.
set -eu
cd workdir
capture=$(mktemp)
trap 'rm -f "$capture"' EXIT HUP INT TERM
status=0
"$KIO_BIN" check 2>"$capture" || status=$?
cat "$capture" >&2
if [ "$status" -ne 13 ]; then exit "$status"; fi
if grep -q 'suggestion: replace with' "$capture"; then
  echo 'unexpected automatic name replacement' >&2
  exit 1
fi
exit 13
