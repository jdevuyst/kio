#!/bin/sh
# Pin canonical `kio fmt` output for `type` alias shapes:
# - Nullary type alias (`Unit`, `Bottom`).
# - Function-type aliases — 0-param (`Func_zero`), 1-param
#   (`Logger`), 2+-param (`Func` breaks to A1).
# - Parametric type alias head — 2+ type params break to A1.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
