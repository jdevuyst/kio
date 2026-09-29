#!/bin/sh
#
# Verify the browser REPL wasm wrapper: format check, clippy, unit
# tests, the wasm32 compile proof, and the public JS wrapper in Node.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT/kio-repl-wasm"

for tool in node wasm-pack; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'kio-repl-wasm: %s not found on PATH; run mise install --locked.\n' "$tool" >&2
    exit 2
  fi
done

sh "$REPO_ROOT/ci/cargo.sh" fmt --check
sh "$REPO_ROOT/ci/cargo.sh" clippy --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" test
sh "$REPO_ROOT/ci/cargo.sh" build --target wasm32-unknown-unknown

# wasm-pack supplies the runner matching this crate's wasm-bindgen version;
# its Cargo subprocesses share the enclosing compiler admission.
if [ "${KIO_CI_SERIALIZE_CARGO:-}" = 1 ]; then
  sh "$REPO_ROOT/ci/schedule.sh" --resource cargo -- \
    sh "$REPO_ROOT/ci/schedule.sh" --resource compiler -- \
    wasm-pack test --node . --lib
else
  sh "$REPO_ROOT/ci/schedule.sh" --resource compiler -- \
    wasm-pack test --node . --lib
fi
