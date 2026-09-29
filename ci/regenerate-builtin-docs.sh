#!/bin/sh
#
# Regenerate the checked-in builtin-module reference from the compiler's
# builtin metadata.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

( cd "$REPO_ROOT/kio-rs" && sh "$REPO_ROOT/ci/cargo.sh" build )

KIO_BIN="$REPO_ROOT/kio-rs/target/debug/kio"
OUT="$REPO_ROOT/docs/guides/builtin-modules.md"
TMP="$OUT.tmp"

trap 'rm -f "$TMP"' EXIT HUP INT TERM
"$KIO_BIN" debug builtin-docs > "$TMP"
mv "$TMP" "$OUT"
trap - EXIT HUP INT TERM
