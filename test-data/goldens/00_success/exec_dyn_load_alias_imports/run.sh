#!/bin/sh
#
# Dyn-load regression (module-alias imports): a guest whose modules use
# `import m as a;` aliases — an alias-qualified guest-fn call, an
# alias-qualified host-fn call, and a module-qualified export lookup —
# loads and runs through dyn_load_prime. Pins the emitted alias grammar
# the loader once rejected as unbound names. Also the canonical
# build-at-test-time round-trip case: the guest is compiled to its Kio'
# image by $KIO_BIN in this very run.
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
  printf 'exec_dyn_load_alias_imports: cannot locate test-data/poc/dyn_load_prime/workdir from %s\n' "$CASE_DIR" >&2
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
cp "$CASE_DIR/workdir/exec_dyn_load_alias_imports.pkg.kio" "$work/exec_dyn_load_alias_imports.pkg.kio"

# Build the guest package to its Kio' image with the impl's own compiler,
# in its own temporary directory (outside the host package root, so the
# host build discovers one package; parallel impls must not share build
# outputs either), then feed the freshly-emitted tree — every module plus
# the manifest — to the host over stdin. Nothing is baked: the case is a
# standing drift alarm between the emitter and the loader.
guestwork=$(mktemp -d)
trap 'rm -rf "$work" "$guestwork"' EXIT INT TERM HUP
cp -R "$CASE_DIR/workdir/guest/." "$guestwork/"
( cd "$guestwork" && "$KIO_BIN" build kio-prime ) >&2

( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

find "$guestwork/out/kio-prime" -name '*.kio' | LC_ALL=C sort | xargs cat \
  | "$KIO_RUNNER" --package-name exec_dyn_load_alias_imports \
      --protocol testapi-dyn-load "$work/out/$KIO_TARGET"
