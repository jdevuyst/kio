#!/bin/sh
#
# Dyn-load regression: a host vendors dyn_load_prime, loads a guest Prime image
# at runtime, and calls exports whose interpreted bodies call hostapi.loop.
# The nested export calls loop from inside a loop step, so the interpreter must
# keep the fold on its own continuation stack.
#
# Environment (set by ci/run-tests.sh): KIO_BIN, KIO_RUNNER, KIO_TARGET.

set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'exec_dyn_load_loop_reentry: cannot locate test-data/poc/dyn_load_prime/workdir from %s\n' "$CASE_DIR" >&2
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
cp "$CASE_DIR/workdir/loop_reentry.pkg.kio" "$work/loop_reentry.pkg.kio"

( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

"$KIO_RUNNER" --protocol testapi-dyn-load "$work/out/$KIO_TARGET"
