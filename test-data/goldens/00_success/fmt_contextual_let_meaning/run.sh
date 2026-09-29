#!/bin/sh
set -eu

# Formatting must preserve a contextual let call used as an operator operand.
fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' 0 1 2 15
cp -R workdir/. "$fixture_root"
cd "$fixture_root"
"$KIO_BIN" check >/dev/null
"$KIO_BIN" fmt >/dev/null
"$KIO_BIN" check >/dev/null
"$KIO_BIN" test >/dev/null
"$KIO_BIN" build "$KIO_TARGET" >/dev/null
"$KIO_RUNNER" --protocol testapi-text "out/$KIO_TARGET"
