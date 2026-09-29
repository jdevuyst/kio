#!/bin/sh
# SUBJECT: JavaScript omits sealed removed host functions while retaining the live host call.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
rm -rf "$scratch/out"

cd "$scratch"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
app_js=$(cat out/js/app.js)

if printf '%s' "$app_js" | grep -q 'log'; then
  printf 'removed host fn log must be absent from the JavaScript module\n' >&2
  exit 1
fi
printf '%s' "$app_js" | grep -q 'host.api.open' || {
  printf 'live host fn open must remain in the JavaScript module\n' >&2
  exit 1
}
