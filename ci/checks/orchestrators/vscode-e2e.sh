#!/bin/sh
#
# Headless VSCode end-to-end tests for the Kio extension.
#
# Driven by `@vscode/test-electron`: downloads (or reuses a
# cached) VS Code, launches it pointed at `tools/vscode-kio/`,
# and runs the Mocha suite at `tools/vscode-kio/test/suite/`
# inside the running editor. Assertions cover extension
# activation, TextMate-floor language registration, and the
# tree-sitter semantic-tokens provider firing on a non-trivial
# document. See `tools/vscode-kio/test/suite/extension.test.ts`.
#
# Catches drift in the *bundled* grammar artifacts (the TextMate
# JSON or the tree-sitter WASM) that the lexical-agreement check
# at `ci/checks/orchestrators/highlight-agreement.sh` doesn't see — the
# agreement check asserts the *grammar definitions* agree; this
# script asserts the bundled artifacts still produce that
# agreement at runtime through VS Code's tokeniser.
#
# Linux-only. The `@vscode/test-electron` matrix on Linux is the
# most reliable target. Needs `node` and `Xvfb` on PATH for the
# headless display.
#
# POSIX sh only.

set -eu

if [ $# -gt 0 ]; then
  case "$1" in
    -h|--help)
      cat <<'EOF'
Usage: sh ci/checks/orchestrators/vscode-e2e.sh

Run the VS Code Kio extension end-to-end suite under Xvfb.
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
EXT_DIR="$REPO_ROOT/tools/vscode-kio"

case "$(uname -s)" in
  Linux) ;;
  *)
    printf 'vscode-e2e: skipping — %s is not a supported test platform\n' "$(uname -s)" >&2
    exit 0
    ;;
esac

if ! command -v node >/dev/null 2>&1; then
  # shellcheck disable=SC2016
  printf 'vscode-e2e: `node` not found on PATH.\n' >&2
  exit 2
fi

if ! command -v Xvfb >/dev/null 2>&1; then
  # Xvfb is GitHub Actions / CI territory. Local developer boxes
  # without Xvfb skip cleanly; the agreement check at
  # `ci/checks/orchestrators/highlight-agreement.sh` covers the grammar layer,
  # and the V.1 build pipeline (`npm run build`) verifies the
  # extension still packages. The runtime-tokenisation check
  # this script provides only fires in the CI matrix.
  # shellcheck disable=SC2016
  printf 'vscode-e2e: skipping — `Xvfb` not on PATH (CI-only test).\n' >&2
  exit 0
fi

if ! command -v tree-sitter >/dev/null 2>&1; then
  # shellcheck disable=SC2016
  printf 'vscode-e2e: `tree-sitter` CLI not found on PATH.\n' >&2
  # shellcheck disable=SC2016
  printf '  install: `mise install --locked tree-sitter`\n' >&2
  exit 2
fi

cd "$EXT_DIR"

# Install / refresh npm deps. `npm ci` if a lock file is
# present (deterministic), else `npm install`.
if [ -f package-lock.json ]; then
  npm ci --no-fund --no-audit --silent
else
  npm install --no-fund --no-audit --silent
fi

# Build the extension (TextMate copy, tree-sitter WASM, esbuild
# bundle, web-tree-sitter runtime WASM) plus the test harness.
npm run build
npm run build:test

XVFB_PID=
XVFB_LOG=
init_orchestrator_tmp vscode-e2e

# Reached only through the trap installed below, which shellcheck's
# reachability analysis does not follow.
# shellcheck disable=SC2317
cleanup_xvfb() {
  status=$?
  if [ -n "$XVFB_PID" ]; then
    kill "$XVFB_PID" >/dev/null 2>&1 || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
  if [ -n "$XVFB_LOG" ] && [ -f "$XVFB_LOG" ]; then
    rm -f "$XVFB_LOG"
  fi
  rm -rf "$ORCHESTRATOR_TMP"
  exit "$status"
}

# Installed before Xvfb starts, not after: the handler is a no-op while
# XVFB_PID is empty, and an interrupt during startup would otherwise
# leave the server behind.
trap cleanup_xvfb EXIT INT TERM HUP

start_xvfb() {
  servernum=99
  while [ -e "/tmp/.X${servernum}-lock" ] || [ -S "/tmp/.X11-unix/X${servernum}" ]; do
    servernum=$((servernum + 1))
  done

  tmp_root=${TMPDIR:-/tmp}
  XVFB_LOG=$(mktemp "$tmp_root/kio-vscode-xvfb.XXXXXX")
  Xvfb ":$servernum" -screen 0 1280x800x24 -nolisten tcp >"$XVFB_LOG" 2>&1 &
  XVFB_PID=$!

  ready=
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    if ! kill -0 "$XVFB_PID" >/dev/null 2>&1; then
      printf 'vscode-e2e: Xvfb exited before becoming ready\n' >&2
      sed -n '1,120p' "$XVFB_LOG" >&2
      return 1
    fi
    if [ -S "/tmp/.X11-unix/X${servernum}" ]; then
      ready=1
      break
    fi
    sleep 1
  done

  if [ -z "$ready" ]; then
    printf 'vscode-e2e: Xvfb did not create display socket :%s\n' "$servernum" >&2
    sed -n '1,120p' "$XVFB_LOG" >&2
    return 1
  fi

  XVFB_DISPLAY=":$servernum"
}

# The `real` launch spawns the actual `kio lsp`, so it needs a `kio`.
if ! command -v cargo >/dev/null 2>&1; then
  # shellcheck disable=SC2016
  printf 'vscode-e2e: `cargo` not found on PATH.\n' >&2
  exit 2
fi
printf 'vscode-e2e: building kio for the real-server launch\n' >&2
KIO_E2E_SERVER="$ORCHESTRATOR_TMP/kio"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO_E2E_SERVER" --all-features --bins
export KIO_E2E_SERVER

start_xvfb

# One launch per server the extension can meet: the mock (editor-side
# wiring), the real `kio lsp` (that the two agree on how to start), and a
# `kio` too old to serve it (that the extension notices before spawning).
# The extension resolves its server once at activation, so these cannot
# share a launch. `@vscode/test-electron` caches its VS Code download
# under `tools/vscode-kio/.vscode-test/`, so only the first pays for it.
status=0
for e2e_mode in mock real stale; do
  printf 'vscode-e2e: launch (%s)\n' "$e2e_mode" >&2
  if ! DISPLAY="$XVFB_DISPLAY" VSCODE_KIO_E2E_MODE="$e2e_mode" \
      node ./test/out/runTest.js; then
    printf 'vscode-e2e: FAILED (%s)\n' "$e2e_mode" >&2
    status=1
  fi
done

exit "$status"
