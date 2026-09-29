#!/bin/sh
# Pin canonical `kio fmt` output for `op` declarations per
# specs/style.md § `op` patterns: one space between pattern tokens
# (including bracket-shaped patterns), compact body spacing, and no final
# separator after the canonical block.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
# Preserve the declared module suffix while formatting a scratch copy.
mkdir -p "$work/fmt_op_patterns" || exit
cp workdir/fmt_op_patterns/main.kio "$work/fmt_op_patterns/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt fmt_op_patterns/main.kio >/dev/null || exit
cat fmt_op_patterns/main.kio
"$KIO_BIN" fmt fmt_op_patterns/main.kio >/dev/null || exit
cat fmt_op_patterns/main.kio
