#!/bin/sh
# Pin canonical `kio fmt` layout for user-operator chains and variadic operator
# literals: short forms stay flat and wide forms break at semantic boundaries.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
