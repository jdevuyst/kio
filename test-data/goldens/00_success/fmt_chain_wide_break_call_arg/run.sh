#!/bin/sh
# A wide `|` chain is used as the leading type argument to `__left__`.
# Brackets delimit that type argument while the formatter breaks the chain
# with leading operators. The second format pass checks that this layout
# stays stable and parses cleanly.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
