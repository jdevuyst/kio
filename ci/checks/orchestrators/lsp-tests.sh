#!/bin/sh
#
# LSP test orchestrator.
#
# Runs the end-to-end LSP smoke tests at kio-rs/tests/lsp_smoke.rs —
# they spawn the real `kio` binary as a subprocess, drive an
# LSP transcript over stdin/stdout, and assert on the published
# diagnostics. The tests cover the v1 surface settled in
# specs/cli.md § `kio lsp`: initialize / shutdown handshake,
# type-error and parse-error diagnostics on didOpen, empty-set
# clear on didSave for a now-clean file.
#
# Distinct from kio-rs's regular `cargo test` job (ci/checks/orchestrators/
# kio-rs.sh) because the LSP harness spawns subprocesses and pipes
# binary framing on stdio — keeping it in its own bucket job means a
# fast-iterating cargo unit-test run isn't paying the LSP spawn cost
# unless this orchestrator runs.
#
# POSIX sh only.

set -eu

if [ $# -gt 0 ]; then
  case "$1" in
    -h|--help)
      cat <<'EOF'
Usage: sh ci/checks/orchestrators/lsp-tests.sh

Run the kio-rs LSP integration suite (kio-rs/tests/lsp_smoke.rs).
EOF
      exit 0
      ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"
init_orchestrator_tmp lsp-tests
trap 'rm -rf "$ORCHESTRATOR_TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$ORCHESTRATOR_TMP/kio" \
  --all-features --bins
KIO_DEBUG_TEST_KIO_BIN=$ORCHESTRATOR_TMP/kio
export KIO_DEBUG_TEST_KIO_BIN

cd "$REPO_ROOT/kio-rs"

# Cargo owns the test binary; every subprocess uses the retained CLI snapshot.
sh "$REPO_ROOT/ci/cargo.sh" test --test lsp_smoke
