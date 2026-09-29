#!/bin/sh
#
# A loaded package may require same-leaf host declarations from different
# declaring modules. The image must retain their exact identities through
# loading, canonical alias resolution, literal evaluation, contract projection,
# adapter preflight, callback dispatch, and instantiation. A source-declared
# callback with interleaved shadowing binders is called both directly and
# through a local alias, pinning canonical alpha-renaming and its erased
# type/value runtime stages.
# Backend-only host attributes
# must not enter dynamic identity.
#
# POSIX sh only.

set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
PKG_MANIFEST="$CASE_DIR/workdir/exec_dyn_load_exact_descriptors.pkg.kio"
if [ ! -d "$PKG_SRC" ]; then
  printf 'exec_dyn_load_exact_descriptors: cannot locate dyn_load_prime sources from %s\n' "$CASE_DIR" >&2
  exit 2
fi

# This exact public surface is the firing guard for the recorded host-compiler
# measurement. Internal loader modules remain ordinary implementation imports;
# exposing them through the bridge recreates thousands of unused host entries.
bridge_entries=$(
  awk '
    /^[[:space:]]*bridge[[:space:]]*\{/ { in_bridge = 1; next }
    in_bridge && /^[[:space:]]*\}/ { exit }
    in_bridge {
      line = $0
      sub(/\/\/.*/, "", line)
      gsub(/[[:space:];]/, "", line)
      if (line != "") print line
    }
  ' "$PKG_MANIFEST" | LC_ALL=C sort
)
expected_bridge_entries='testapi
testapi/arith
testapi/fmt
testapi/io
testapi/iter
testapi/main
testapi/scalar
testapi/text'
if [ "$bridge_entries" != "$expected_bridge_entries" ]; then
  printf 'exec_dyn_load_exact_descriptors: bridge surface escaped its measured exact module set:\n%s\n' \
    "$bridge_entries" >&2
  exit 2
fi

work=$(mktemp -d)
guestwork=$(mktemp -d)
trap 'rm -rf "$work" "$guestwork"' EXIT INT TERM HUP

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
cp "$CASE_DIR/workdir/exec_dyn_load_exact_descriptors.pkg.kio" "$work/exec_dyn_load_exact_descriptors.pkg.kio"
cp -R "$CASE_DIR/workdir/guest/." "$guestwork/"

( cd "$work" && "$KIO_BIN" test >/dev/null )
( cd "$guestwork" && "$KIO_BIN" build kio-prime ) >&2
( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

find "$guestwork/out/kio-prime" -name '*.kio' | LC_ALL=C sort | xargs cat \
  | "$KIO_RUNNER" --package-name exec_dyn_load_exact_descriptors \
      --protocol testapi-dyn-load "$work/out/$KIO_TARGET"
