#!/bin/sh
#
# A raw image must obey Kio's specified declaration-order boundary:
# an earlier local host signature or transparent alias cannot acquire meaning
# from a later local newtype. Imported and qualified provider surfaces remain
# complete and are covered by the broader exact-descriptor cases.

set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'exec_dyn_load_late_local_newtype: cannot locate dyn_load_prime sources from %s\n' "$CASE_DIR" >&2
  exit 2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP

for f in "$PKG_SRC"/*.kio; do
  base=$(basename "$f")
  case "$base" in
    dyn_load_prime.pkg.kio) continue ;;
  esac
  cp "$f" "$work/$base"
done
mkdir -p "$work/testapi"
cp "$PKG_SRC"/testapi/*.kio "$work/testapi/"
cp -R "$PKG_SRC/list" "$work/list"
cp -R "$PKG_SRC/loader" "$work/loader"
cp -R "$PKG_SRC/elab" "$work/elab"

cp "$CASE_DIR/workdir/testapi/main.kio" "$work/testapi/main.kio"
cp "$CASE_DIR/workdir/exec_dyn_load_late_local_newtype.pkg.kio" "$work/exec_dyn_load_late_local_newtype.pkg.kio"

( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

"$KIO_RUNNER" --protocol testapi-dyn-load "$work/out/$KIO_TARGET" < /dev/null
