#!/bin/sh
# Gate canonical form for tracked Kio outside test-data/, whose sources
# are covered separately by the per-case fmt-canonical check. Parse
# errors (exit 11) are outside a formatting invariant; every other
# unexpected formatter failure remains a gate failure.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

usage() {
  printf '%s\n' \
    'Usage: fmt-canonical-tree.sh [--help]' \
    '' \
    'Check canonical formatting for tracked Kio sources outside test-data/.'
}

if [ $# -gt 1 ]; then
  printf 'fmt-canonical-tree: expected at most one argument\n' >&2
  exit 2
fi

if [ $# -gt 0 ]; then
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'fmt-canonical-tree: unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
fi

cd "$REPO_ROOT"

scratch=$(mktemp -d)
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_fmt_canonical_tree() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_fmt_canonical_tree EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
export ORCHESTRATOR_TMP="$scratch"
KIO_BIN="$scratch/kio"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO_BIN" --all-features --bins
[ -x "$KIO_BIN" ] || {
  printf 'fmt-canonical-tree: kio binary missing at %s\n' "$KIO_BIN" >&2
  exit 2
}

paths="$scratch/paths"
results="$scratch/results"
: >"$results"

# NUL-delimited index paths preserve every filename accepted by Git.
git ls-files -z -- '*.kio' ':(exclude)test-data/' >"$paths"
if [ ! -s "$paths" ]; then
  printf 'fmt-canonical-tree: found no tracked *.kio outside test-data/\n' >&2
  exit 2
fi

export KIO_BIN results
xargs_status=0
# shellcheck disable=SC2016 # the child shell expands these exported variables
xargs -0 -n 1 sh -c '
  f=$1
  case "$f" in
    -*) checked_path=./$f ;;
    *) checked_path=$f ;;
  esac
  status=0
  output=$("$KIO_BIN" fmt --check "$checked_path" 2>&1) || status=$?
  case "$status" in
    0)
      printf C >>"$results"
      ;;
    11)
      printf S >>"$results"
      ;;
    60)
      printf D >>"$results"
      printf "fmt-canonical-tree: non-canonical tracked source: %s\n" "$f" >&2
      exit 42
      ;;
    *)
      printf E >>"$results"
      printf "fmt-canonical-tree: kio fmt --check exited %s for %s\n" "$status" "$f" >&2
      if [ -n "$output" ]; then
        printf "%s\n" "$output" >&2
      fi
      exit 43
      ;;
  esac
' sh <"$paths" || xargs_status=$?

case "$xargs_status" in
  0) ;;
  123) exit 1 ;; # one or more classified file failures
  *)
    printf 'fmt-canonical-tree: path traversal failed (xargs exit %s)\n' "$xargs_status" >&2
    exit 1
    ;;
esac

checked=$(tr -cd C <"$results" | wc -c | tr -d '[:space:]')
skipped=$(tr -cd S <"$results" | wc -c | tr -d '[:space:]')
printf 'fmt-canonical-tree: OK (%s canonical, %s non-parsing skipped)\n' "$checked" "$skipped"
