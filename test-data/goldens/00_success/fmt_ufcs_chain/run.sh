#!/bin/sh
# Pin canonical `kio fmt` output for UFCS chains. Short chains stay
# inline; wide chains break to leading-arrow multi-line with the
# innermost receiver on its own line and each `.>segment(args)` at
# +2 indent. Bare zero-arg segments emit without parens in either
# layout. Wide args within one segment fall back to A1 leading-
# comma independently of the surrounding chain break.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
