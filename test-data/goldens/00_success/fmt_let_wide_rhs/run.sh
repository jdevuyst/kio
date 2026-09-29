#!/bin/sh
# Pin canonical `kio fmt` output for wide-RHS `let X = E;` cases.
# Flat short let stays inline. Wide let breaks at `=`, value on
# its own line at +2 indent, `;` trailing the value's last token.
# Wide let inside a chain composes: chain stacks vertically and
# the wide let additionally breaks internally.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
