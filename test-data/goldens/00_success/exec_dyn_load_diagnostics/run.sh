#!/bin/sh
#
# Dyn-load regression (diagnostics): each class of malformed image the
# loader rejects yields a structured diagnostic naming the construct —
# the owning module and fn for a body failure, the cycle's modules, the
# unresolvable import's module and item, the position of a lexical
# error — instead of the silent acceptance or the one fixed
# "malformed image" string the loader once produced. The malformed
# texts are baked by nature: they cannot be produced by `kio build`.
#
# The host vendors the live test-data/poc/dyn_load_prime/workdir/ package
# (the single source of truth — no committed fork) alongside this case's
# own testapi/main host module and manifest, exactly as
# exec_dyn_load_integration does.
#
# Environment (set by ci/run-tests.sh): KIO_BIN, KIO_RUNNER, KIO_TARGET.
#
# POSIX sh only.

set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'exec_dyn_load_diagnostics: cannot locate test-data/poc/dyn_load_prime/workdir from %s\n' "$CASE_DIR" >&2
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
cp "$CASE_DIR/workdir/exec_dyn_load_diagnostics.pkg.kio" "$work/exec_dyn_load_diagnostics.pkg.kio"

( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

"$KIO_RUNNER" --protocol testapi-dyn-load "$work/out/$KIO_TARGET" < /dev/null
