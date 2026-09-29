#!/bin/sh
#
# Generative conservation check for `kio fmt` comment preservation.
#
# The contract (specs/style.md § Comments): a `//` comment is legal
# anywhere and is *never dropped* by `kio fmt` — faithful at
# boundaries, hoisted otherwise, and source order is preserved across
# any hoist. This check enforces that property generatively, the way
# decision 2 of the fix is meant to be enforced (a test, not a runtime
# guard):
#
#   1. Generate a small batch of programs with kio-gen.
#   2. For each `*.kio` file, canonicalise it with `kio fmt` (so the
#      injection lands on clean token-boundary lines), then append a
#      globally sequential `// comment<N>` to (nearly) every line.
#   3. Run `kio fmt` on the injected file.
#   4. Extract the `comment<N>` markers from the output and assert
#      ALL injected markers are present AND appear in strictly
#      increasing order. The in-order check is the strong part: it
#      catches drops *and* reordering, so the interstitial-hoist
#      fallback can never leapfrog a comment.
#
# A `//` comment runs to end-of-line, and it is appended *last* on its
# line, so it can't swallow following tokens. Kio has no multi-line
# string literals (a newline inside a `"..."` is a lex error), so
# every source line ends at a token boundary and the append is safe.
# A post-injection PARSE failure is a finding (the formatter must
# accept a comment anywhere a comment is legal), not a skip.
#
# This drives the `kio` and `kio-gen` binaries directly over an
# on-disk batch — it is not a `cargo test` and not the run-tests.sh
# exec/exit-code path; kio-gen is used only as a diverse program
# source. `ci/all.sh` auto-discovers this bucket, so placing the
# script here gates it. Sibling of generative-tests.sh.
#
# Usage:
#   sh ci/checks/orchestrators/fmt-comment-conservation.sh \
#     [--count=<N>] [--seed=<N>]
#
# --count   number of programs to generate (default: small; the
#           property is exercised by program *shape* diversity, which
#           a handful covers — a large batch only slows the gate).
# --seed    deterministic seed; default random, logged on failure.
#
# Unknown arguments are ignored so `ci/all.sh`'s orchestrator-arg
# injection (which targets the run-tests.sh orchestrators) is harmless
# here. POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

COUNT=8
SEED=

while [ $# -gt 0 ]; do
  case "$1" in
    --count=*) COUNT=${1#--count=} ;;
    --count) shift; [ $# -gt 0 ] && COUNT=$1 ;;
    --seed=*) SEED=${1#--seed=} ;;
    --seed) shift; [ $# -gt 0 ] && SEED=$1 ;;
    -h|--help)
      sed -n '2,/^set -eu/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    # Tolerate run-tests.sh-style flags ci/all.sh forwards to its
    # corpus orchestrators; this one drives kio fmt directly. The
    # value-bearing `--impls VALUE` form drops its value here; the
    # `--impls=VALUE` form needs no extra shift.
    --impls) if [ $# -gt 1 ]; then shift; fi ;;
    --impls=*) ;;
    *) ;;
  esac
  shift
done

batch=$(mktemp -d)
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_fmt_comment_conservation() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$batch"
  exit "$cleanup_status"
}
trap cleanup_fmt_comment_conservation EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

export ORCHESTRATOR_TMP="$batch"
KIO_BIN="$batch/kio"
KIO_GEN_BIN="$REPO_ROOT/ci/infra/kio-gen-rs/target/debug/kio-gen"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO_BIN" --all-features --bins
( cd "$REPO_ROOT/ci/infra/kio-gen-rs" && sh "$REPO_ROOT/ci/cargo.sh" build )

[ -x "$KIO_BIN" ] || { printf 'fmt-comment-conservation: kio binary missing at %s\n' "$KIO_BIN" >&2; exit 2; }
[ -x "$KIO_GEN_BIN" ] || { printf 'fmt-comment-conservation: kio-gen binary missing at %s\n' "$KIO_GEN_BIN" >&2; exit 2; }

if [ -n "$SEED" ]; then
  "$KIO_GEN_BIN" --output "$batch" --count "$COUNT" --seed "$SEED" >/dev/null
else
  SEED=$(awk 'BEGIN { srand(); print int(rand() * 2147483647) }')
  "$KIO_GEN_BIN" --output "$batch" --count "$COUNT" --seed "$SEED" >/dev/null
fi

printf 'fmt-comment-conservation: count=%s seed=%s\n' "$COUNT" "$SEED"

failures=0
checked=0

# `check_file <path>`: canonicalise, inject numbered comments, re-format,
# and assert every injected marker survives in increasing order.
check_file() {
  file=$1
  start_n=$2

  # 1. Canonicalise so the injection lands on clean lines. A file that
  #    doesn't even format cleanly is out of scope for this property
  #    (the generator can emit deliberate error cases); skip it.
  if ! "$KIO_BIN" fmt - <"$file" >"$file.canon" 2>/dev/null; then
    rm -f "$file.canon"
    echo 0
    return 0
  fi

  # 2. Append a globally sequential `// comment<N>` to each non-blank
  #    line. Blank lines are left alone (a `// comment` on its own
  #    would just be another leading comment, adding no signal).
  n=$start_n
  : >"$file.injected"
  while IFS= read -r line || [ -n "$line" ]; do
    if [ -n "$line" ]; then
      printf '%s // comment%s\n' "$line" "$n" >>"$file.injected"
      n=$((n + 1))
    else
      printf '\n' >>"$file.injected"
    fi
  done <"$file.canon"
  injected_count=$((n - start_n))

  # 3. Re-format the injected file. A parse failure here is a FINDING:
  #    a comment is legal on any line, so the formatter must accept it.
  if ! "$KIO_BIN" fmt - <"$file.injected" >"$file.reformatted" 2>"$file.err"; then
    printf 'fmt-comment-conservation: FAIL (parse after injection) %s\n' "$file" >&2
    sed 's/^/  /' "$file.err" >&2 || :
    rm -f "$file.canon" "$file.injected" "$file.reformatted" "$file.err"
    echo "-1"
    return 0
  fi

  # 4. Extract marker numbers in output order and assert all present,
  #    strictly increasing. grep -o pulls each `comment<N>` token; the
  #    awk pass checks the count and the monotonic order.
  markers=$(grep -oE 'comment[0-9]+' "$file.reformatted" | sed 's/comment//')
  result=$(printf '%s\n' "$markers" | START="$start_n" EXPECT="$injected_count" FILE="$file" awk '
    BEGIN { prev = -1; count = 0; ok = 1 }
    /^[0-9]+$/ {
      v = $1 + 0
      count++
      if (v <= prev) { printf "order-violation at marker %d (after %d) in %s\n", v, prev, ENVIRON["FILE"] > "/dev/stderr"; ok = 0 }
      prev = v
    }
    END {
      expect = ENVIRON["EXPECT"] + 0
      if (count != expect) { printf "count mismatch in %s: expected %d markers, found %d\n", ENVIRON["FILE"], expect, count > "/dev/stderr"; ok = 0 }
      print (ok ? "ok" : "bad")
    }
  ')

  rm -f "$file.canon" "$file.injected" "$file.reformatted" "$file.err"
  if [ "$result" = ok ]; then
    echo "$injected_count"
  else
    echo "-1"
  fi
}

# Walk every generated `*.kio` file. A global counter keeps marker
# numbers unique across files so an accidental cross-file leak would
# also show as an order violation.
global_n=1
for file in $(find "$batch" -name '*.kio' -type f | sort); do
  res=$(check_file "$file" "$global_n")
  if [ "$res" = "-1" ]; then
    failures=$((failures + 1))
    printf 'fmt-comment-conservation: FAIL %s\n' "$file" >&2
  elif [ "$res" -gt 0 ]; then
    checked=$((checked + 1))
    global_n=$((global_n + res))
  fi
done

printf 'fmt-comment-conservation: checked %s file(s), %s failure(s)\n' "$checked" "$failures"

if [ "$failures" -ne 0 ]; then
  printf 'fmt-comment-conservation: reproduce with --seed=%s --count=%s\n' "$SEED" "$COUNT" >&2
  exit 1
fi

if [ "$checked" -eq 0 ]; then
  printf 'fmt-comment-conservation: no formattable files in batch (seed=%s)\n' "$SEED" >&2
  exit 1
fi

exit 0
