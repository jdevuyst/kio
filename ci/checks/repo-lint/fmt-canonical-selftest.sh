#!/bin/sh
# Exercise formatter status handling without building a compiler.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
CHECK="$REPO_ROOT/ci/checks/per-case/fmt-canonical.sh"
CHECK_SHELL=${1:-sh}
scratch=$(mktemp -d)
cleanup_fmt_canonical_selftest() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_fmt_canonical_selftest EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

mkdir -p "$scratch/case/workdir"
cat >"$scratch/kio" <<'SH'
#!/bin/sh
case "$FMT_TEST_OUTCOME" in
  clean) exit 0 ;;
  parse) printf 'malformed input\n' >&2; exit 11 ;;
  dirty) printf 'workdir/main.kio\n'; exit 60 ;;
  empty-dirty) exit 60 ;;
  internal) printf 'internal formatter failure\n' >&2; exit 1 ;;
  io) printf 'cannot read input\n' >&2; exit 1 ;;
  usage) printf 'unexpected option\n' >&2; exit 2 ;;
  type) printf 'unexpected type error\n' >&2; exit 14 ;;
  signal) printf 'formatter interrupted\n' >&2; kill -TERM "$$" ;;
  *) exit 127 ;;
esac
SH
chmod +x "$scratch/kio"
export KIO_BIN="$scratch/kio"
cd "$scratch/case"

for mode in check update; do
  KIO_TEST_UPDATE=0
  [ "$mode" != update ] || KIO_TEST_UPDATE=1
  export KIO_TEST_UPDATE
  for outcome in clean parse dirty empty-dirty internal io usage type signal; do
    FMT_TEST_OUTCOME=$outcome
    export FMT_TEST_OUTCOME
    status=0
    "$CHECK_SHELL" "$CHECK" >"$scratch/stdout" 2>"$scratch/stderr" || status=$?
    case "$outcome" in
      clean|parse) expected=0 ;;
      *) expected=1 ;;
    esac
    if [ "$status" -ne "$expected" ]; then
      printf 'fmt-canonical-selftest: %s/%s returned %s, expected %s\n' \
        "$mode" "$outcome" "$status" "$expected" >&2
      exit 1
    fi
    if [ "$expected" -eq 1 ]; then
      test -s "$scratch/stderr" || {
        printf 'fmt-canonical-selftest: %s/%s lost diagnostics\n' "$mode" "$outcome" >&2
        exit 1
      }
      case "$outcome" in
        dirty) diagnostic='workdir/main.kio' ;;
        empty-dirty) diagnostic='exited 60' ;;
        internal) diagnostic='internal formatter failure' ;;
        io) diagnostic='cannot read input' ;;
        usage) diagnostic='unexpected option' ;;
        type) diagnostic='unexpected type error' ;;
        signal) diagnostic='formatter interrupted' ;;
      esac
      grep -F "$diagnostic" "$scratch/stderr" >/dev/null || {
        printf 'fmt-canonical-selftest: %s/%s missing diagnostic: %s\n' \
          "$mode" "$outcome" "$diagnostic" >&2
        exit 1
      }
    fi
  done
done
printf 'fmt-canonical-selftest: OK\n'
