#!/bin/sh
#
# Validate website-owned Kio examples without running host artifacts.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../.." && pwd)
EXAMPLE_DIR="$REPO_ROOT/website/examples/hello-world"
KIO_BIN=${KIO_BIN:-"$REPO_ROOT/kio-rs/target/debug/kio"}

if [ ! -x "$KIO_BIN" ]; then
  (
    cd "$REPO_ROOT/kio-rs"
    sh "$REPO_ROOT/ci/cargo.sh" build --quiet --bin kio
  )
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

work="$scratch/hello-world"
mkdir -p "$work"
cp "$EXAMPLE_DIR/hello.pkg.kio" "$EXAMPLE_DIR/hello.kio" "$work/"

(
  cd "$work"
  "$KIO_BIN" check
  "$KIO_BIN" fmt
)

for file in hello.pkg.kio hello.kio; do
  if ! diff -u "$EXAMPLE_DIR/$file" "$work/$file"; then
    printf 'website example is not kio fmt canonical: %s\n' "$file" >&2
    exit 1
  fi
done
