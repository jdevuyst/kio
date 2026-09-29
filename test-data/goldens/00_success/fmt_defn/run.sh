#!/bin/sh
# Pin canonical `kio fmt` output for `fn` shapes:
# - 0-param stays single-line.
# - 1-param `fn` (just `[A]`) stays single-line.
# - 2+-param `fn` breaks to A1 leading-comma layout.
#
# Strategy mirrors fmt_canonical: workdir is single-line minified
# input, copied to scratch so `kio fmt` (which rewrites in place)
# doesn't canonicalise the tracked source.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
