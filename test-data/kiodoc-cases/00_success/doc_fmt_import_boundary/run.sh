#!/bin/sh
set -eu

case_tmp=$(mktemp -d)
trap 'rm -rf "$case_tmp"' EXIT HUP INT TERM
cp input.md pkg.pkg.kio "$case_tmp"/
cd "$case_tmp"

"$KIO_BIN" doc check input.md
cp input.md original.md

if "$KIO_BIN" doc fmt input.md >format.out 2>format.err; then
  code=0
else
  code=$?
fi
if [ "$code" -ne 70 ]; then
  printf 'expected mapping exit 70, got %s\n' "$code" >&2
  exit 1
fi
grep -F 'cannot be mapped back' format.err >/dev/null
cmp input.md original.md
"$KIO_BIN" doc check input.md
printf 'preserved imports\n'
