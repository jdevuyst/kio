#!/bin/sh
# Pin canonical `kio fmt` output for load-bearing parens around
# `&` / `|` type chains. Outer parens are stripped only when they
# don't affect parsing — chains where the parens distinguish AST
# shape (mixing, function-type child, left-leaning same-op,
# function-type param list) keep them.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
