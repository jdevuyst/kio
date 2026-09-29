#!/bin/sh
#
# Dyn-load regression (binding introductions): fresh Kio' text rejects a second
# ordinary type, value, or module-alias introduction even for the same provider.
# Positional alias members remain ordinary, and the fixed intrinsics block is
# idempotent. Loading checks structure and parses bodies; it does not typecheck
# or execute the guest bodies in these images.
#
# The host vendors the live dyn_load_prime POC implementation. The malformed
# image is authored directly because ordinary Kio source rejects the collision
# before it can emit Kio'.
#
# Environment (set by ci/run-tests.sh): KIO_BIN, KIO_RUNNER, KIO_TARGET.
#
# POSIX sh only.

set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'exec_dyn_load_binding_origins: cannot locate test-data/poc/dyn_load_prime/workdir from %s\n' "$CASE_DIR" >&2
  exit 2
fi

work=$(mktemp -d)
cleanup_binding_origins() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$work"
  exit "$cleanup_status"
}
trap cleanup_binding_origins EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

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
cp "$CASE_DIR/workdir/exec_dyn_load_binding_origins.pkg.kio" "$work/exec_dyn_load_binding_origins.pkg.kio"

( cd "$work" && "$KIO_BIN" test >/dev/null )
( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

"$KIO_RUNNER" --protocol testapi-dyn-load "$work/out/$KIO_TARGET" < /dev/null
