#!/bin/sh
set -eu

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

cp -R workdir "$scratch/consumer"
cp -R library "$scratch/library"
cd "$scratch/consumer"
"$KIO_BIN" dep fetch --force >/dev/null
"$KIO_BIN" check >/dev/null
formatted=$("$KIO_BIN" fmt - <widget/relay.kio)
again=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
if [ "$formatted" != "$again" ]; then
  printf '%s\n' 'materialized import formatting was not idempotent' >&2
  exit 1
fi
printf '%s\n' "$formatted"
