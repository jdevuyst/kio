#!/bin/sh
# Exercise selected-output discovery and freshness through both real consumers.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
if command -v mise >/dev/null 2>&1 && NODE=$(cd "$REPO_ROOT" && mise which node 2>/dev/null) && [ -n "$NODE" ]; then
  :
else
  NODE=$(command -v node)
fi
exec "$NODE" "$REPO_ROOT/ci/infra/typed-output-check-selftest.mjs" "$REPO_ROOT" "$@"
