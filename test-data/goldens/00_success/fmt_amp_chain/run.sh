#!/bin/sh
# Pin canonical `kio fmt` output for `&` and `|` type chains.
# These are expression-shape lists (not declaration-shape), so
# they stay single-line if they fit. Today every chain emits
# inline regardless of length — line-length-driven breaks are
# deferred per specs/style.md § Out of scope.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
