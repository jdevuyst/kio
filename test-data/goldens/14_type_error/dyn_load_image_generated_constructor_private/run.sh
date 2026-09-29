#!/bin/sh
# A loaded Image is authority produced by validation, so the public loader API
# exposes neither a generated constructor nor its explicitly named raw members.
# Each spelling is checked separately so one diagnostic cannot mask another.
# The case checks the live POC source rather than a copied loader fixture.
set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'dyn_load_image_generated_constructor_private: cannot locate dyn_load_prime from %s\n' "$CASE_DIR" >&2
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

cp "$CASE_DIR/workdir/dyn_load_image_generated_constructor_private.pkg.kio" "$work/"

run_probe() {
  member=$1
  sed "s/Image\\.mk/Image.$member/g" "$CASE_DIR/workdir/testapi/main.kio" > "$work/testapi/main.kio"
  status=0
  (cd "$work" && "$KIO_BIN" check) > "$work/$member.stdout" 2> "$work/$member.stderr" || status=$?
  cat "$work/$member.stdout"
  cat "$work/$member.stderr" >&2
  if [ "$status" -ne 14 ]; then
    printf 'dyn_load_image_generated_constructor_private: %s exited %s, expected 14\n' "$member" "$status" >&2
    exit 1
  fi
}

run_probe mk
run_probe seal_image_state
run_probe open_image_state

exit 14
