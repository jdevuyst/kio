#!/bin/sh
# Pin canonical `kio fmt` output for explicit nominal declaration shapes:
# - Nullary nominal (no type params, plain payload).
# - Single-param nominal (head stays inline).
# - Two-param nominal.
# - Recursive nominal using `rec` for self-reference.
# Member lists pair the fixed mk and get roles.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
