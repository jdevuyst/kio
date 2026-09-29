#!/bin/sh
# Pin canonical `kio fmt` output for `newtype` shapes:
# - Nullary newtype (no type params, plain payload).
# - Single-param newtype (head stays inline).
# - Two-param newtype.
# - Recursive newtype using `rec` for self-reference.
# Member lists pair a constructor with a projector.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
