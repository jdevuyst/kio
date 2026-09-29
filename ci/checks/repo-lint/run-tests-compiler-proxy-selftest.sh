#!/bin/sh
#
# Verify compiler-proxy admission for custom corpus scripts.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
if [ -z "${KIO_CI_SCHEDULER_BIN:-}" ]; then
  KIO_CI_SCHEDULER_BIN=$(sh "$REPO_ROOT/ci/schedule.sh" --prepare)
  export KIO_CI_SCHEDULER_BIN
fi

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/run-tests-compiler-proxy.XXXXXX") || {
  printf 'run-tests-compiler-proxy-selftest: cannot make scratch directory\n' >&2
  exit 2
}
pids=
cleanup() {
  trap - EXIT INT TERM HUP
  for pid in $pids; do
    kill -TERM "$pid" >/dev/null 2>&1 || :
  done
  for pid in $pids; do
    wait "$pid" >/dev/null 2>&1 || :
  done
  rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

mkdir -p \
  "$scratch/bin with space" \
  "$scratch/cache" \
  "$scratch/cases/bucket/native" \
  "$scratch/tmp"

cat >"$scratch/bin with space/kio" <<'EOF'
#!/bin/sh
set -eu
case "${1:-} ${2:-}" in
  'cache --help'|'cache clear') exit 0 ;;
  'debug kio-prime-roundtrip-package')
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*) exit 109 ;;
    esac
    : >"$KIO_TEST_KIO_ROUNDTRIP_DIRECT"
    ;;
  'debug tokens')
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*) ;;
      *) exit 110 ;;
    esac
    : >"$KIO_TEST_KIO_TOKENS_ADMITTED"
    ;;
esac
exit 0
EOF

cat >"$scratch/bin with space/runner" <<'EOF'
#!/bin/sh
[ -z "${KIO_CI_SCHEDULE_READINESS_HOOK_PROGRAM:-}" ] || exit 106
[ -z "${KIO_CI_SCHEDULE_READINESS_HOOK_ARG_COUNT:-}" ] || exit 107
[ -z "${KIO_CI_SCHEDULE_READINESS_HOOK_ARG_0:-}" ] || exit 108
exit 0
EOF

cat >"$scratch/bin with space/rustup" <<'EOF'
#!/bin/sh
set -eu
if [ "${0##*/}" = rustc ] && [ "$#" = 2 ] &&
   [ "$1" = --version ] && [ "$2" = --verbose ]; then
  printf '%s\n' "$#" "$@" >"$KIO_TEST_RUSTC_QUERY_ROOT/query-argv"
  IFS= read -r query_stdin
  [ "$query_stdin" = 'query stdin' ] || exit 111
  case ",${KIO_CI_SCHEDULE_HELD:-}," in
    *,compiler,*)
      sh "$KIO_TEST_SCHEDULE" --resource compiler -- \
        sh -c ': >"$KIO_TEST_RUSTC_QUERY_ROOT/query-inherited"'
      ;;
    *) : >"$KIO_TEST_RUSTC_QUERY_ROOT/query-direct" ;;
  esac
  printf 'query stdout\n'
  printf 'query stderr\n' >&2
  exit "${KIO_TEST_RUSTC_QUERY_STATUS:-0}"
fi
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 91 ;;
esac
case "${0##*/}" in
  cargo)
    : >"$KIO_TEST_CARGO_ADMITTED"
    rustc --version
    printf 'query stdin\n' | rustc --version --verbose \
      >"$KIO_TEST_RUSTC_QUERY_ROOT/inherited.stdout" \
      2>"$KIO_TEST_RUSTC_QUERY_ROOT/inherited.stderr"
    ;;
  rustc)
    : >"$KIO_TEST_RUSTC_ADMITTED"
    printf '%s\n' "$#" "$@" >>"$KIO_TEST_RUSTC_QUERY_ROOT/admitted-argv"
    ;;
  *) exit 92 ;;
esac
EOF
ln -s rustup "$scratch/bin with space/cargo"
ln -s rustup "$scratch/bin with space/rustc"

cat >"$scratch/bin with space/go" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}" in
  -C) shift 2 ;;
  -C=*) shift ;;
  -future-flag)
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*) ;;
      *) exit 102 ;;
    esac
    : >"$KIO_TEST_GO_UNKNOWN_ADMITTED"
    exit 0
    ;;
esac
case "${1:-}" in
  env)
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*) exit 93 ;;
    esac
    : >"$KIO_TEST_GO_ENV_DIRECT"
    ;;
  run)
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*) ;;
      *) exit 94 ;;
    esac
    : >"$KIO_TEST_GO_RUN_ADMITTED"
    ;;
  future-compile)
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*) ;;
      *) exit 104 ;;
    esac
    : >"$KIO_TEST_GO_UNKNOWN_SUBCOMMAND_ADMITTED"
    ;;
  list)
    case " $* " in
      *" -export "*)
        case ",${KIO_CI_SCHEDULE_HELD:-}," in
          *,compiler,*) ;;
          *) exit 96 ;;
        esac
        : >"$KIO_TEST_GO_LIST_ADMITTED"
        ;;
      *) exit 97 ;;
    esac
    ;;
  tool)
    case " $* " in
      *" dist "*)
        case ",${KIO_CI_SCHEDULE_HELD:-}," in
          *,compiler,*) ;;
          *) exit 103 ;;
        esac
        : >"$KIO_TEST_GO_DIST_ADMITTED"
        ;;
      *" compile "*)
        case ",${KIO_CI_SCHEDULE_HELD:-}," in
          *,compiler,*) ;;
          *) exit 98 ;;
        esac
        : >"$KIO_TEST_GO_TOOL_ADMITTED"
        ;;
      *" cover "*)
        case ",${KIO_CI_SCHEDULE_HELD:-}," in
          *,compiler,*) exit 99 ;;
        esac
        : >"$KIO_TEST_GO_TOOL_DIRECT"
        ;;
      *" future-compiler "*)
        case ",${KIO_CI_SCHEDULE_HELD:-}," in
          *,compiler,*) ;;
          *) exit 105 ;;
        esac
        : >"$KIO_TEST_GO_UNKNOWN_TOOL_ADMITTED"
        ;;
      *) exit 100 ;;
    esac
    ;;
  *) exit 95 ;;
esac
EOF

cat >"$scratch/native-tool" <<'EOF'
#!/bin/sh
set -eu
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 101 ;;
esac
: >"$KIO_TEST_NATIVE_MARKERS/${0##*/}"
EOF

for tool in javac swiftc ghc; do
  cp "$scratch/native-tool" "$scratch/bin with space/$tool"
done

cat >"$scratch/cases/bucket/native/run.sh" <<'EOF'
#!/bin/sh
set -eu
  printf 'query stdin\n' | rustc --version --verbose \
    >"$KIO_TEST_RUSTC_QUERY_ROOT/direct.stdout" \
    2>"$KIO_TEST_RUSTC_QUERY_ROOT/direct.stderr"
  cmp "$KIO_TEST_RUSTC_QUERY_ROOT/expected.stdout" "$KIO_TEST_RUSTC_QUERY_ROOT/direct.stdout"
  cmp "$KIO_TEST_RUSTC_QUERY_ROOT/expected.stderr" "$KIO_TEST_RUSTC_QUERY_ROOT/direct.stderr"
  if printf 'query stdin\n' | KIO_TEST_RUSTC_QUERY_STATUS=23 rustc --version --verbose \
      >"$KIO_TEST_RUSTC_QUERY_ROOT/nonzero.stdout" \
      2>"$KIO_TEST_RUSTC_QUERY_ROOT/nonzero.stderr"; then
    exit 112
  else
    query_status=$?
  fi
  [ "$query_status" = 23 ] || exit 113
  cmp "$KIO_TEST_RUSTC_QUERY_ROOT/expected.stdout" "$KIO_TEST_RUSTC_QUERY_ROOT/nonzero.stdout"
  cmp "$KIO_TEST_RUSTC_QUERY_ROOT/expected.stderr" "$KIO_TEST_RUSTC_QUERY_ROOT/nonzero.stderr"
  go env
  go tool cover
  "$KIO_BIN" debug kio-prime-roundtrip-package "$0" js
  "$KIO_BIN" debug tokens "$0"
  cargo check
  rustc fixture.rs
  rustc --future-query
  rustc --version --verbose fixture.rs
  rustc --verbose --version
  go run .
  go -C fixture run .
  go -future-flag fixture
  go future-compile fixture
  go list -export .
  go tool compile fixture.go
  go tool dist list
  go tool future-compiler fixture.go
  go tool -C fixture compile fixture.go
  go tool -overlay fixture.json compile fixture.go
  go tool -modfile fixture.mod compile fixture.go
  javac Fixture.java
swiftc Fixture.swift
ghc Fixture.hs
EOF

cat >"$scratch/hold" <<'EOF'
#!/bin/sh
set -eu
: >"$KIO_TEST_READY"
while [ ! -e "$KIO_TEST_RELEASE" ]; do
  sleep 0.01
done
EOF

chmod +x \
  "$scratch/bin with space/kio" \
  "$scratch/bin with space/runner" \
  "$scratch/bin with space/rustup" \
  "$scratch/bin with space/go" \
  "$scratch/bin with space/javac" \
  "$scratch/bin with space/swiftc" \
  "$scratch/bin with space/ghc" \
  "$scratch/cases/bucket/native/run.sh" \
  "$scratch/hold"

printf '0\n' >"$scratch/cases/bucket/native/expected.exit"
: >"$scratch/cases/bucket/native/expected.stdout"
: >"$scratch/cases/bucket/native/expected.stderr.ignore"
printf 'query stdout\n' >"$scratch/expected.stdout"
printf 'query stderr\n' >"$scratch/expected.stderr"
printf '%s\n' 2 --version --verbose >"$scratch/expected.query-argv"
printf '%s\n' 1 --version 1 fixture.rs 1 --future-query \
  3 --version --verbose fixture.rs 2 --verbose --version \
  >"$scratch/expected.admitted-argv"

wait_for_file() {
  wff_file=$1
  wff_attempt=0
  while [ ! -e "$wff_file" ]; do
    wff_attempt=$((wff_attempt + 1))
    [ "$wff_attempt" -lt 500 ] || return 1
    sleep 0.01
  done
}

wait_for_pending() {
  wfp_dir=$1
  wfp_attempt=0
  while ! has_pending_claim "$wfp_dir"; do
    wfp_attempt=$((wfp_attempt + 1))
    [ "$wfp_attempt" -lt 500 ] || return 1
    sleep 0.01
  done
}

has_pending_claim() {
  hpc_dir=$1
  for hpc_claim in "$hpc_dir"/*.pending; do
    [ -f "$hpc_claim" ] && return 0
  done
  return 1
}

state=$scratch/state
ready=$scratch/holder-ready
release=$scratch/holder-release
KIO_CI_SCHEDULE_DIR="$state" \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_READY="$ready" \
  KIO_TEST_RELEASE="$release" \
  sh "$REPO_ROOT/ci/schedule.sh" --resource compiler -- "$scratch/hold" &
holder_pid=$!
pids="$pids $holder_pid"
wait_for_file "$ready" || {
  printf 'run-tests-compiler-proxy-selftest: holder was not admitted\n' >&2
  exit 1
}

log=$scratch/run.log
PATH="$scratch/bin with space:$PATH" \
  KIO_CI_SCHEDULE_DIR="$state" \
  KIO_CI_SCHEDULE_JOBS=1 \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_CARGO_ADMITTED="$scratch/cargo-admitted" \
  KIO_TEST_RUSTC_ADMITTED="$scratch/rustc-admitted" \
  KIO_TEST_RUSTC_QUERY_ROOT="$scratch" \
  KIO_TEST_SCHEDULE="$REPO_ROOT/ci/schedule.sh" \
  KIO_TEST_KIO_ROUNDTRIP_DIRECT="$scratch/kio-roundtrip-direct" \
  KIO_TEST_KIO_TOKENS_ADMITTED="$scratch/kio-tokens-admitted" \
  KIO_TEST_GO_ENV_DIRECT="$scratch/go-env-direct" \
  KIO_TEST_GO_RUN_ADMITTED="$scratch/go-run-admitted" \
  KIO_TEST_GO_UNKNOWN_ADMITTED="$scratch/go-unknown-admitted" \
  KIO_TEST_GO_UNKNOWN_SUBCOMMAND_ADMITTED="$scratch/go-unknown-subcommand-admitted" \
  KIO_TEST_GO_UNKNOWN_TOOL_ADMITTED="$scratch/go-unknown-tool-admitted" \
  KIO_TEST_GO_LIST_ADMITTED="$scratch/go-list-admitted" \
  KIO_TEST_GO_TOOL_ADMITTED="$scratch/go-tool-admitted" \
  KIO_TEST_GO_DIST_ADMITTED="$scratch/go-dist-admitted" \
  KIO_TEST_GO_TOOL_DIRECT="$scratch/go-tool-direct" \
  KIO_TEST_NATIVE_MARKERS="$scratch" \
  TMPDIR="$scratch/tmp" \
  sh "$REPO_ROOT/ci/run-tests.sh" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def="name=fixture,kio=$scratch/bin with space/kio,runner=$scratch/bin with space/runner,target=js" \
    --jobs=1 \
    --compiler-jobs=1 >"$log" 2>&1 &
harness_pid=$!
pids="$pids $harness_pid"

wait_for_pending "$state/compiler/claims" || {
  cat "$log" >&2
  printf 'run-tests-compiler-proxy-selftest: custom compiler did not wait for the occupied permit\n' >&2
  exit 1
}
if [ -e "$scratch/kio-tokens-admitted" ] ||
   [ -e "$scratch/cargo-admitted" ] || [ -e "$scratch/go-run-admitted" ]; then
  printf 'run-tests-compiler-proxy-selftest: custom compiler exceeded capacity one\n' >&2
  exit 1
fi
if [ ! -e "$scratch/query-direct" ] ||
   [ ! -e "$scratch/kio-roundtrip-direct" ] ||
   [ ! -e "$scratch/go-env-direct" ] || [ ! -e "$scratch/go-tool-direct" ]; then
  printf 'run-tests-compiler-proxy-selftest: direct command was admitted or did not run\n' >&2
  exit 1
fi

: >"$release"
if ! wait "$holder_pid"; then
  printf 'run-tests-compiler-proxy-selftest: holder failed\n' >&2
  exit 1
fi
if ! wait "$harness_pid"; then
  cat "$log" >&2
  printf 'run-tests-compiler-proxy-selftest: admitted custom compiler case failed\n' >&2
  exit 1
fi
pids=

cmp "$scratch/expected.query-argv" "$scratch/query-argv"
cmp "$scratch/expected.admitted-argv" "$scratch/admitted-argv"
cmp "$scratch/expected.stdout" "$scratch/inherited.stdout"
cmp "$scratch/expected.stderr" "$scratch/inherited.stderr"
test -e "$scratch/query-inherited"

for marker in kio-tokens-admitted cargo-admitted rustc-admitted \
  go-run-admitted go-unknown-admitted \
  go-unknown-subcommand-admitted go-unknown-tool-admitted go-list-admitted \
  go-tool-admitted go-dist-admitted javac swiftc ghc; do
  [ -e "$scratch/$marker" ] || {
    printf 'run-tests-compiler-proxy-selftest: missing %s marker\n' "$marker" >&2
    exit 1
  }
done
if find "$state/compiler/claims" -type f -print -quit | grep -q .; then
  printf 'run-tests-compiler-proxy-selftest: custom compiler left a lease behind\n' >&2
  exit 1
fi

printf 'run-tests-compiler-proxy-selftest: ok\n'
