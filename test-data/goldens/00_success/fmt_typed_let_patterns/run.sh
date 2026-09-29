#!/bin/sh
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
# Preserve the declared module suffix while formatting a scratch copy.
mkdir -p "$work/fmt_typed_let_patterns" || exit
cp workdir/fmt_typed_let_patterns/main.kio "$work/fmt_typed_let_patterns/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt fmt_typed_let_patterns/main.kio >/dev/null || exit
cat fmt_typed_let_patterns/main.kio
"$KIO_BIN" fmt fmt_typed_let_patterns/main.kio >/dev/null || exit
cat fmt_typed_let_patterns/main.kio
