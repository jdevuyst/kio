#!/bin/sh
# Pin canonical `kio fmt` over bare same-operator chain inputs.
# Surface Kio accepts `A & B & C` and `A | B | C` as type
# expressions (no outer parens required); fmt is a no-op once
# the input is canonical bare-chain shape.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
