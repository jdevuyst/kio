#!/bin/sh
#
# Reference-tokenization corpus check.
#
# Builds the `kio` binary, walks `test-data/highlight-corpus/`, and for
# each fixture diffs `kio debug tokens source.kio` against the
# fixture's `expected.tokens.json`. Asserts that the dump is stable,
# byte-for-byte, against a curated corpus exercising every kind in the
# canonical token-kind vocabulary (defined by `TokenKind` in
# `kio-rs/src/tokens.rs`).
#
# Usage:
#   sh ci/checks/orchestrators/highlight-tokens.sh [-u|--update-expected]
#                                          [<filter>...]
#
# -u, --update-expected   Overwrite each fixture's
#                         `expected.tokens.json` with the actual
#                         output. Use after intentional vocabulary
#                         or classification changes.
#                         in the environment.
# <filter>                Optional substring patterns; only fixtures
#                         whose directory name matches at least one
#                         filter are visited. Without filters, every
#                         fixture under `test-data/highlight-corpus/`
#                         runs.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"
CORPUS_DIR="$REPO_ROOT/test-data/highlight-corpus"

update=0
filters=""
for arg in "$@"; do
  case "$arg" in
    -u|--update-expected) update=1 ;;
    -h|--help)
      sed -n '2,30p' "$0"
      exit 0
      ;;
    -*)
      printf 'unknown option: %s\n' "$arg" >&2
      exit 2
      ;;
    *)
      filters="$filters $arg"
      ;;
  esac
done

scratch=$(mktemp -d)
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_highlight_tokens() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_highlight_tokens EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

export ORCHESTRATOR_TMP="$scratch"
KIO_BIN="$scratch/kio"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO_BIN" --all-features --bins

if [ ! -d "$CORPUS_DIR" ]; then
  printf 'highlight-tokens: corpus directory missing: %s\n' "$CORPUS_DIR" >&2
  exit 2
fi

passes=0
fails=0
failed_names=""

# Walk the corpus in deterministic order. Each fixture is a
# subdirectory containing `source.kio` and `expected.tokens.json`.
# `kio debug tokens` writes its JSON dump to a scratch file directly
# (rather than through `$()` capture, which would strip the trailing
# newline and mismatch the golden by exactly that one byte).
#
# Iterating via a temp file + `while IFS= read -r` keeps fixture
# paths intact even if a directory name carries whitespace —
# unlikely under our naming convention but cheap insurance.
fixture_list="$scratch/fixtures"
find "$CORPUS_DIR" -mindepth 1 -maxdepth 1 -type d | sort > "$fixture_list"
while IFS= read -r fixture; do
  name=$(basename "$fixture")
  source="$fixture/source.kio"
  expected="$fixture/expected.tokens.json"
  if [ ! -f "$source" ]; then
    # Allow non-fixture dirs (e.g. README scratch); skip silently.
    continue
  fi

  # Apply filter — if any filter is given, the fixture must match
  # at least one substring.
  if [ -n "$filters" ]; then
    matched=0
    for f in $filters; do
      case "$name" in
        *"$f"*) matched=1; break ;;
      esac
    done
    [ "$matched" = 1 ] || continue
  fi

  actual="$scratch/$name.tokens.json"
  if ! "$KIO_BIN" debug tokens "$source" > "$actual"; then
    printf 'FAIL: %s — kio debug tokens exited non-zero\n' "$name" >&2
    fails=$((fails + 1))
    failed_names="$failed_names $name"
    continue
  fi

  if [ "$update" = 1 ]; then
    cp "$actual" "$expected"
    printf 'updated: %s\n' "$name"
    passes=$((passes + 1))
    continue
  fi

  if [ ! -f "$expected" ]; then
    printf 'FAIL: %s — missing expected.tokens.json (run with -u to create)\n' "$name" >&2
    fails=$((fails + 1))
    failed_names="$failed_names $name"
    continue
  fi

  if diff -u "$expected" "$actual" >/dev/null 2>&1; then
    passes=$((passes + 1))
  else
    printf 'FAIL: %s — output differs from expected.tokens.json:\n' "$name" >&2
    diff -u "$expected" "$actual" >&2 || true
    fails=$((fails + 1))
    failed_names="$failed_names $name"
  fi
done < "$fixture_list"

printf '\nhighlight-tokens: %d passed, %d failed\n' "$passes" "$fails"
if [ "$fails" != 0 ]; then
  printf 'FAILED:\n' >&2
  for n in $failed_names; do
    printf '  %s\n' "$n" >&2
  done
  exit 1
fi
