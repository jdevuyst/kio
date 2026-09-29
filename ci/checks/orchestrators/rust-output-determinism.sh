#!/bin/sh
#
# Build a small Rust-output slice twice from cold Kio caches and assert
# that the emitted Rust source is byte-stable. This catches checked-term
# alpha-name drift before it turns into runner-cache churn.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

CASES='
00_success/exec_expr_eval
00_success/exec_factorial
00_success/exec_functor_dictionary
00_success/exec_functor_identity
00_success/exec_functor_list
'

usage() {
  cat <<EOF
Usage: sh $0

Build selected Rust-output golden cases twice, clearing each case's Kio
cache and output directory before each build, then compare
out/rust/src/lib.rs hashes.
EOF
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  "")
    ;;
  *)
    printf 'error: unknown argument: %s\n' "$1" >&2
    exit 2
    ;;
esac

if [ "${KIO_CI_SCHEDULE:-}" != DISABLE ] &&
   [ -z "${KIO_CI_SCHEDULER_BIN:-}" ]; then
  KIO_CI_SCHEDULER_BIN=$(sh "$REPO_ROOT/ci/schedule.sh" --prepare) || exit $?
  export KIO_CI_SCHEDULER_BIN
fi

tmp=$(mktemp -d)
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_rust_output_determinism() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$tmp"
  exit "$cleanup_status"
}
trap cleanup_rust_output_determinism EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

export ORCHESTRATOR_TMP="$tmp"
KIO_BIN="$tmp/kio"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO_BIN" --all-features --bins

safe_case_name() {
  printf '%s' "$1" | tr '/[:space:]' '__'
}

build_pass() {
  pass=$1
  hash_file=$2
  snapshot_dir=$tmp/pass-$pass
  mkdir -p "$snapshot_dir"

  printf 'rust-output-determinism: pass %s\n' "$pass"
  for case_name in $CASES; do
    case_dir=$REPO_ROOT/test-data/goldens/$case_name
    workdir=$case_dir/workdir
    lib=$workdir/out/rust/src/lib.rs

    if [ ! -d "$workdir" ]; then
      printf 'error: missing case workdir: %s\n' "$workdir" >&2
      exit 1
    fi

    rm -rf "$workdir/out/rust"
    (
      cd "$workdir"
      "$KIO_BIN" cache clear >/dev/null 2>&1 || true
      sh "$REPO_ROOT/ci/schedule.sh" --resource compiler -- \
        "$KIO_BIN" build rust
    )

    if [ ! -f "$lib" ]; then
      printf 'error: rust build did not write %s\n' "$lib" >&2
      exit 1
    fi

    hash=$(cksum <"$lib")
    printf '%s  %s\n' "$hash" "$case_name" >>"$hash_file"
    cp "$lib" "$snapshot_dir/$(safe_case_name "$case_name").lib.rs"
  done
}

hashes_1=$tmp/hashes-1
hashes_2=$tmp/hashes-2
build_pass 1 "$hashes_1"
build_pass 2 "$hashes_2"

if cmp -s "$hashes_1" "$hashes_2"; then
  printf 'rust-output-determinism: pass\n'
  exit 0
fi

printf 'error: emitted Rust lib.rs hashes changed across repeated builds\n' >&2
printf '\nfirst pass:\n' >&2
sed 's/^/  /' "$hashes_1" >&2
printf '\nsecond pass:\n' >&2
sed 's/^/  /' "$hashes_2" >&2

for case_name in $CASES; do
  safe=$(safe_case_name "$case_name")
  first=$tmp/pass-1/$safe.lib.rs
  second=$tmp/pass-2/$safe.lib.rs
  if ! cmp -s "$first" "$second"; then
    printf '\nfirst changed case: %s\n' "$case_name" >&2
    diff -u "$first" "$second" | sed -n '1,160p' >&2 || true
    break
  fi
done

exit 1
