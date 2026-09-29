#!/bin/sh
#
# Dynamic loading revalidates the package bridge contract from the image text:
# every bridge glob matches at least one module, and every host-bearing module
# transitively reachable from a bridged module is itself bridged. The
# hidden-host chain crosses both a selective-import edge and a qualified-import
# edge. A separate ordering probe pins stable diagnostics when multiple hidden
# host modules are reachable through different import forms. The control admits
# the host-bearing module while leaving the host-free middle module internal. A
# present manifest without a bridge and an explicit empty bridge both export
# nothing, while a true manifest-less image retains the documented all-public
# fallback.
#
# The host vendors the live dyn_load_prime POC sources, then replaces only its
# own entry module and package manifest.
#
# POSIX sh only.

set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'exec_dyn_load_bridge_contract_validation: cannot locate dyn_load_prime sources from %s\n' "$CASE_DIR" >&2
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
cp "$CASE_DIR/workdir/exec_dyn_load_bridge_contract_validation.pkg.kio" \
  "$work/exec_dyn_load_bridge_contract_validation.pkg.kio"

( cd "$work" && "$KIO_BIN" test >/dev/null )
( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

"$KIO_RUNNER" --protocol testapi-dyn-load "$work/out/$KIO_TARGET" < /dev/null
