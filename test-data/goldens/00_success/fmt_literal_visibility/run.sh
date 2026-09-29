#!/bin/sh
set -eu

format_root=$(mktemp -d)
trap 'rm -rf "$format_root"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$format_root/api"
cp workdir/api/inner.kio "$format_root/api/inner.kio"
cd "$format_root"
"$KIO_BIN" fmt >/dev/null
cat api/inner.kio
"$KIO_BIN" fmt >/dev/null
cat api/inner.kio
