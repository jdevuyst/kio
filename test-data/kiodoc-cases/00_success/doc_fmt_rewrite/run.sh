#!/bin/sh
set -eu

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
cp input.md pkg.pkg.kio "$tmp"/
cd "$tmp"

set +e
"$KIO_BIN" doc fmt --check input.md >dirty.out
code=$?
set -e

if [ "$code" -ne 60 ]; then
  printf 'expected dirty check exit 60, got %s\n' "$code" >&2
  exit 1
fi
grep -Fx input.md dirty.out >/dev/null

"$KIO_BIN" doc fmt input.md
"$KIO_BIN" doc fmt --check input.md

grep -F 'pub fn greet() -> . { print("hi\n") }' input.md >/dev/null
grep -F 'build {' input.md >/dev/null
