#!/bin/sh
# `kio fmt` cuddles a nested `} else { if … }` chain into the
# canonical `} else if cond' { … }` form. The two inputs below
# verify:
#
# 1. A chain that fits within the 100-col budget formats to one
#    inline `if a { x } else if b { y } else { z }`.
# 2. The cuddle parses round-trip — the formatted output, re-run
#    through `kio fmt`, is a fixed point of itself.
#
# Same harness shape as the other `fmt_*` goldens: copy the
# deliberately-minified `workdir/main.kio` to a scratch dir so the
# tracked source stays minified, then run `kio fmt` twice and
# `cat` the result twice to show idempotence.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
