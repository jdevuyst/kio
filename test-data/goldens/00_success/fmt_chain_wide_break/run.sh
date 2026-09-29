#!/bin/sh
# A wide `|` chain breaks the surrounding `type` declaration to the
# leading-operator A1 layout: `=` at end of head line, each chain
# item on its own line at +2 prefixed with `| `, `;` at the same
# indent on its own line.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
