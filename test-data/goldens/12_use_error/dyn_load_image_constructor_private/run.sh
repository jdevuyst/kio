#!/bin/sh
# Loaded images and their export tables are authority produced by validation,
# so another module cannot import any raw construction helper. Each helper is
# checked in its own compiler invocation so one diagnostic cannot mask another.
# The case checks the live POC source rather than a copied loader fixture.
set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'dyn_load_image_constructor_private: cannot locate dyn_load_prime from %s\n' "$CASE_DIR" >&2
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

cp "$CASE_DIR/workdir/dyn_load_image_constructor_private.pkg.kio" "$work/"

run_probe() {
  helper=$1
  sed "s/mk_image/$helper/g" "$CASE_DIR/workdir/testapi/main.kio" > "$work/testapi/main.kio"
  status=0
  (cd "$work" && "$KIO_BIN" check) > "$work/$helper.stdout" 2> "$work/$helper.stderr" || status=$?
  cat "$work/$helper.stdout"
  cat "$work/$helper.stderr" >&2
  if [ "$status" -ne 12 ]; then
    printf 'dyn_load_image_constructor_private: %s exited %s, expected 12\n' "$helper" "$status" >&2
    exit 1
  fi
  # shellcheck disable=SC2016 # backticks are literal diagnostic text
  if grep -Fq 'add `pub`' "$work/$helper.stderr"; then
    printf 'dyn_load_image_constructor_private: %s told the importer to weaken provider visibility\n' "$helper" >&2
    exit 1
  fi
}

run_probe mk_image
run_probe un_image
run_probe mk_export_info
run_probe export_infos_nil
run_probe export_infos_cons

exit 12
