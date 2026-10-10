#!/bin/sh
# A healthy generated file must not mask a formatter failure on another file.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch=$(mktemp -d)
cleanup_fmt_comment_conservation_selftest() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_fmt_comment_conservation_selftest EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fixture=$scratch/repo
mkdir -p "$fixture/ci/checks/orchestrators/lib" "$fixture/kio-rs" \
  "$fixture/ci/infra/kio-gen-rs/target/debug"
cp "$REPO_ROOT/ci/checks/orchestrators/fmt-comment-conservation.sh" \
  "$fixture/ci/checks/orchestrators/"
cat >"$fixture/ci/checks/orchestrators/lib/common.sh" <<'SH'
build_corpus_tool_binary() { cp "$FMT_TEST_KIO" "$4"; }
SH
cat >"$fixture/ci/cargo.sh" <<'SH'
set -eu
case "$*" in
  build) ;;
  'test --all-features --test fmt_comment_conservation')
    test "$KIO_DEBUG_FMT_RAW_GENERATED_DIR" = "$ORCHESTRATOR_TMP"
    test "$KIO_DEBUG_FMT_RAW_SEED" = 1
    test -f "$KIO_DEBUG_FMT_RAW_GENERATED_DIR/healthy.kio"
    test ! -e "$KIO_DEBUG_FMT_RAW_GENERATED_DIR/healthy.kio.canon"
    printf 'raw test invoked\n' >>"$FMT_TEST_RAW_CALLS"
    if [ "${FMT_TEST_RAW_FAILURE:-0}" = 1 ]; then
      printf 'raw conservation failed\n' >&2
      exit 1
    fi
    ;;
  *) exit 2 ;;
esac
SH
cat >"$fixture/ci/infra/kio-gen-rs/target/debug/kio-gen" <<'SH'
#!/bin/sh
set -eu
test "$1" = --output
test "$3 $4 $5 $6 $7 $8" = '--count 2 --seed 1 --valid-fraction 1.0'
printf 'module healthy;\nfn unit() { () }\n' >"$2/healthy.kio"
printf 'module broken;\n// BAD\n' >"$2/broken.kio"
SH
FMT_TEST_KIO=$scratch/kio
FMT_TEST_RAW_CALLS=$scratch/raw-calls
export FMT_TEST_KIO FMT_TEST_RAW_CALLS
cat >"$FMT_TEST_KIO" <<'SH'
#!/bin/sh
set -eu
test "$*" = 'fmt -'
source=$(cat)
case "$source" in
  *BAD*)
    printf 'formatter outcome: %s\n' "$FMT_TEST_OUTCOME" >&2
    case "$FMT_TEST_OUTCOME" in
      parse) exit 11 ;;
      internal|io) exit 1 ;;
      usage) exit 2 ;;
      type) exit 14 ;;
      signal) kill -TERM "$$" ;;
    esac
    ;;
esac
if [ "${FMT_TEST_SHIFT_MARKERS:-0}" = 1 ]; then
  printf '%s\n' "$source" | sed 's/comment/comment9/g'
  exit 0
fi
printf '%s\n' "$source"
SH
chmod +x "$FMT_TEST_KIO" "$fixture/ci/infra/kio-gen-rs/target/debug/kio-gen"

for outcome in parse internal io usage type signal; do
  FMT_TEST_OUTCOME=$outcome
  export FMT_TEST_OUTCOME
  status=0
  sh "$fixture/ci/checks/orchestrators/fmt-comment-conservation.sh" \
    --count=2 --seed=1 >"$scratch/output" 2>&1 || status=$?
  expected=1
  [ "$outcome" != parse ] || expected=0
  if [ "$status" -ne "$expected" ]; then
    printf 'fmt-comment-conservation-selftest: %s returned %s, expected %s\n' \
      "$outcome" "$status" "$expected" >&2
    exit 1
  fi
  if [ "$expected" -eq 1 ]; then
    grep -F "formatter outcome: $outcome" "$scratch/output" >/dev/null
    grep -F '1 failure(s)' "$scratch/output" >/dev/null
  else
    grep -F 'checked 1 file(s), 0 failure(s)' "$scratch/output" >/dev/null
  fi
done
test "$(wc -l <"$FMT_TEST_RAW_CALLS" | tr -d ' ')" = 6

FMT_TEST_OUTCOME=parse
FMT_TEST_SHIFT_MARKERS=1
export FMT_TEST_OUTCOME FMT_TEST_SHIFT_MARKERS
status=0
sh "$fixture/ci/checks/orchestrators/fmt-comment-conservation.sh" \
  --count=2 --seed=1 >"$scratch/output" 2>&1 || status=$?
if [ "$status" -ne 1 ]; then
  printf 'fmt-comment-conservation-selftest: shifted marker IDs returned %s, expected 1\n' "$status" >&2
  exit 1
fi
grep -F 'marker mismatch' "$scratch/output" >/dev/null

FMT_TEST_RAW_FAILURE=1
export FMT_TEST_RAW_FAILURE
status=0
sh "$fixture/ci/checks/orchestrators/fmt-comment-conservation.sh" \
  --count=2 --seed=1 >"$scratch/output" 2>&1 || status=$?
test "$status" = 1
grep -F 'raw conservation failed' "$scratch/output" >/dev/null
printf 'fmt-comment-conservation-selftest: OK\n'
