#!/bin/sh
#
# Check that the checked-in builtin-module reference matches the compiler's
# builtin metadata renderer.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"
init_orchestrator_tmp builtin-docs
KIO_BIN="$ORCHESTRATOR_TMP/kio"
EXPECTED="$REPO_ROOT/docs/guides/builtin-modules.md"
TMP="$ORCHESTRATOR_TMP/builtin-docs.md"

# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_builtin_docs() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -f "$TMP"
  rm -rf "$ORCHESTRATOR_TMP"
  exit "$cleanup_status"
}
trap cleanup_builtin_docs EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO_BIN" --all-features --bins
"$KIO_BIN" debug builtin-docs > "$TMP"
if ! cmp -s "$TMP" "$EXPECTED"; then
  printf 'FAIL: docs/guides/builtin-modules.md is stale; run sh ci/regenerate-builtin-docs.sh\n' >&2
  diff -u "$EXPECTED" "$TMP" >&2 || true
  exit 1
fi
