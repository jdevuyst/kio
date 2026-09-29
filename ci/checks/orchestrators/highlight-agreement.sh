#!/bin/sh
#
# Cross-tokenizer agreement check.
#
# Verifies — over the curated fixtures under
# `test-data/highlight-corpus/` — that the three Kio tokenizers agree:
#
#   1. `kio debug tokens` (the reference; `kio-rs/src/tokens.rs`)
#   2. tree-sitter, via `tools/tree-sitter-kio/grammar.js` and
#      an in-process WASM parser
#   3. TextMate, via `tools/textmate-kio/kio.tmLanguage.json` loaded
#      through the `vscode-textmate` engine (same engine VS Code
#      uses internally)
#
# Tree-sitter must match (1) exactly, including every kind and token boundary.
# TextMate may remain neutral only when later-line evidence disambiguates a
# role: its line-oriented engine cannot revise earlier lines from that evidence.
# Every positive TextMate classification must agree; the focused raw-scope
# fixtures also pin provable roles and paired ambiguous prefixes.
#
# Implementation: a Node.js driver at `ci/infra/highlight-agreement-js/`
# walks each fixture and runs the three tokenizers. The script
# itself just builds the kio binary, regenerates the tree-sitter
# parser, builds the parser WASM, installs Node deps, and invokes
# the driver. CI gains
# Node and `tree-sitter-cli` as toolchain dependencies; both are
# already required for the wider syntax-highlighting track.
#
# POSIX sh only.

set -eu

if [ $# -gt 0 ]; then
  case "$1" in
    -h|--help)
      cat <<'EOF'
Usage: sh ci/checks/orchestrators/highlight-agreement.sh

Verifies cross-tokenizer agreement (kio debug tokens / tree-sitter /
TextMate) over test-data/highlight-corpus/. See the header comment for
the full contract.
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
CORPUS_DIR="$REPO_ROOT/test-data/highlight-corpus"
TS_GRAMMAR_DIR="$REPO_ROOT/tools/tree-sitter-kio"
TM_GRAMMAR_PATH="$REPO_ROOT/tools/textmate-kio/kio.tmLanguage.json"
AGREEMENT_DIR="$REPO_ROOT/ci/infra/highlight-agreement-js"
TS_WASM_PATH="$AGREEMENT_DIR/parsers/tree-sitter-kio.wasm"

if ! command -v tree-sitter >/dev/null 2>&1; then
  # shellcheck disable=SC2016 # backticks are literal text
  printf 'highlight-agreement: `tree-sitter` CLI not found on PATH.\n' >&2
  # shellcheck disable=SC2016
  printf '  install: `mise install --locked tree-sitter`\n' >&2
  exit 2
fi

if ! command -v node >/dev/null 2>&1; then
  # shellcheck disable=SC2016
  printf 'highlight-agreement: `node` not found on PATH.\n' >&2
  exit 2
fi

init_orchestrator_tmp highlight-agreement
trap 'rm -rf "$ORCHESTRATOR_TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
KIO_BIN="$ORCHESTRATOR_TMP/kio"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO_BIN" --all-features --bins

# Regenerate the tree-sitter parser. The committed `src/parser.c`
# is what the CI compiles against, but regenerating here catches
# `grammar.js` edits that weren't paired with a regenerated
# parser. Quiet on success.
( cd "$TS_GRAMMAR_DIR" && tree-sitter generate )

# Build the parser WASM loaded by the Node driver. The file is
# generated from the local grammar on every run so agreement always
# checks the same parser source as `tree-sitter generate`.
mkdir -p "$AGREEMENT_DIR/parsers"
( cd "$TS_GRAMMAR_DIR" && tree-sitter build --wasm -o "$TS_WASM_PATH" )

# Install / refresh Node dependencies for the driver. `npm ci` if
# `package-lock.json` is present (deterministic), else `npm install`.
if [ -f "$AGREEMENT_DIR/package-lock.json" ]; then
  ( cd "$AGREEMENT_DIR" && npm ci --no-fund --no-audit --silent )
else
  ( cd "$AGREEMENT_DIR" && npm install --no-fund --no-audit --silent )
fi

# Run the driver. It walks the corpus and reports fixture failures
# plus a summary; non-zero exit on any failure.
node "$AGREEMENT_DIR/check.mjs" \
  "$CORPUS_DIR" \
  "$TS_WASM_PATH" \
  "$TM_GRAMMAR_PATH" \
  "$KIO_BIN"
