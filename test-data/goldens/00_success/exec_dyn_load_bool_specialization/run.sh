#!/bin/sh
# Resolve each conditional's explicit Boolean type through ordinary aliases.
set -eu

case_dir=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$case_dir" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
test -d "$PKG_SRC"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP

for source in "$PKG_SRC"/*.kio; do
  name=$(basename "$source")
  case "$name" in dyn_load_prime.pkg.kio) continue ;; esac
  cp "$source" "$work/$name"
done
for directory in testapi list loader macros; do
  cp -R "$PKG_SRC/$directory" "$work/$directory"
done
cp "$case_dir/workdir/testapi/main.kio" "$work/testapi/main.kio"
cp "$case_dir/workdir/bool_specialization.pkg.kio" "$work/bool_specialization.pkg.kio"
( cd "$work" && "$KIO_BIN" check ) >&2
( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2
"$KIO_RUNNER" --package-name bool_specialization --protocol testapi-dyn-load "$work/out/$KIO_TARGET" < /dev/null
