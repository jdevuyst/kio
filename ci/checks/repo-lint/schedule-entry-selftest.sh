#!/bin/sh
# Verify the shell boundary around the native Kio CI scheduler.
#
# Queueing, capacity, lease lifetime, signals, and process-tree
# supervision are native scheduler responsibilities covered by Rust tests.

set -eu

unset \
  KIO_CI_SCHEDULE \
  KIO_CI_SCHEDULE_DIR \
  KIO_CI_SCHEDULE_HELD \
  KIO_CI_SCHEDULE_JOBS \
  KIO_CI_SCHEDULE_COMPILER_JOBS \
  KIO_CI_SCHEDULE_LEASE_FDS \
  KIO_CI_SCHEDULER_BIN \
  KIO_CI_SERIALIZE_CARGO \
  RUSTC_WRAPPER \
  RUSTC_WORKSPACE_WRAPPER \
  CARGO_BUILD_RUSTC_WRAPPER \
  CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
  KIO_TEST_RUNNER_COMPILER_WRAPPER \
  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd -P)
SCHEDULE_SH=${SCHEDULE_SH:-"$REPO_ROOT/ci/schedule.sh"}
CARGO_SH=$REPO_ROOT/ci/cargo.sh
ORCHESTRATOR_COMMON=$REPO_ROOT/ci/checks/orchestrators/lib/common.sh

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/schedule-entry-selftest.XXXXXX") || {
  printf 'schedule-entry-selftest: cannot make scratch directory\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

fail() {
  printf 'schedule-entry-selftest: %s\n' "$1" >&2
  exit 1
}

assert_same() {
  as_expected=$1
  as_actual=$2
  as_context=$3
  if ! cmp -s "$as_expected" "$as_actual"; then
    printf 'schedule-entry-selftest: %s\n' "$as_context" >&2
    diff -u "$as_expected" "$as_actual" >&2 || :
    exit 1
  fi
}

native_path() {
  np_shell=$(CDPATH='' cd -- "$1" && pwd -P) || return 2
  case "${OS:-}:${MSYSTEM:-}" in
    Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*) cygpath -m "$np_shell" ;;
    *) printf '%s\n' "$np_shell" ;;
  esac
}

mkdir -p "$scratch/bin"

# This fixture implements only the native CLI transport contract needed to
# observe the facade. It deliberately contains no admission policy.
cat >"$scratch/fake-scheduler" <<'EOF'
#!/bin/sh
set -eu

case "${1:-}" in
  available-parallelism)
    [ "$#" -eq 1 ] || exit 95
    printf '%s\n' 4
    exit 0
    ;;
  self-test)
    [ "$#" -eq 1 ] || exit 95
    printf '%s\n' 'fixture self-test'
    exit 0
    ;;
  supervise)
    fs_log=$KIO_TEST_SCHEDULER_LOG_BASE.supervise
    : >"$fs_log"
    for fs_arg in "$@"; do
      printf '%s\n' "$fs_arg" >>"$fs_log"
    done
    exit 0
    ;;
  run) ;;
  *) exit 96 ;;
esac

resource=
expect_resource=
for fs_arg in "$@"; do
  if [ -n "$expect_resource" ]; then
    resource=$fs_arg
    expect_resource=
  elif [ "$fs_arg" = --resource ]; then
    expect_resource=1
  fi
done
[ -n "$resource" ] || exit 94

fs_log=$KIO_TEST_SCHEDULER_LOG_BASE.$resource
: >"$fs_log"
for fs_arg in "$@"; do
  printf '%s\n' "$fs_arg" >>"$fs_log"
done
printf '%s\n' "${KIO_CI_SCHEDULE_DIR:-}" >"$fs_log.root"

shift
while [ "$#" -gt 0 ] && [ "$1" != -- ]; do
  shift
done
[ "${1:-}" = -- ] || exit 93
shift
[ "$#" -gt 0 ] || exit 93

# The child marker is part of the native CLI contract. Propagating it lets the
# Cargo facade exercise its cargo-then-compiler re-entry without simulating a
# queue or lock.
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,"$resource",*) ;;
  ,,) KIO_CI_SCHEDULE_HELD=$resource ;;
  *) KIO_CI_SCHEDULE_HELD=${KIO_CI_SCHEDULE_HELD},$resource ;;
esac
export KIO_CI_SCHEDULE_HELD
exec "$@"
EOF
chmod +x "$scratch/fake-scheduler"

cat >"$scratch/record-command" <<'EOF'
#!/bin/sh
set -eu
{
  printf '%s\n' "$#"
  for rc_arg in "$@"; do
    printf '%s\n' "$rc_arg"
  done
} >"$KIO_TEST_COMMAND_OUTPUT"
printf '%s\n' "${KIO_CI_SCHEDULE_DIR:-}" >"$KIO_TEST_COMMAND_ROOT"
EOF
chmod +x "$scratch/record-command"

cat >"$scratch/stdin-state" <<'EOF'
#!/bin/sh
if (exec 7<&0) 2>/dev/null; then
  printf '%s\n' open
else
  printf '%s\n' closed
fi >"$KIO_TEST_STDIN_OUTPUT"
EOF
chmod +x "$scratch/stdin-state"

cat >"$scratch/bin/git" <<'EOF'
#!/bin/sh
set -eu
[ "$#" -eq 5 ]
[ "$1" = -C ]
[ "$3" = rev-parse ]
[ "$4" = --path-format=absolute ]
[ "$5" = --git-common-dir ]
printf '%s\n' "$KIO_TEST_GIT_COMMON"
EOF
chmod +x "$scratch/bin/git"

cat >"$scratch/bin/cargo" <<'EOF'
#!/bin/sh
set -eu
{
  pwd -P
  printf '%s\n' "$#"
  for fc_arg in "$@"; do
    printf '%s\n' "$fc_arg"
  done
  printf '%s\n' "${KIO_CI_SCHEDULE_HELD:-}"
} >"$KIO_TEST_CARGO_OUTPUT"
EOF
chmod +x "$scratch/bin/cargo"

cat >"$scratch/bin/sccache" <<'EOF'
#!/bin/sh
set -eu
[ "$#" -eq 1 ] && [ "$1" = --dist-status ]
[ "${SCCACHE_IDLE_TIMEOUT:-}" = 0 ]
[ -z "${KIO_CI_SCHEDULE_HELD:-}" ]
[ -z "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ]
printf '%s\n' "$1" >>"$KIO_TEST_SCCACHE_LOG"
EOF
chmod +x "$scratch/bin/sccache"

mkdir -p "$scratch/observer bin"
cat >"$scratch/observer bin/rustc" <<'EOF'
#!/bin/sh
set -eu
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 87 ;;
esac
case "${OS:-}:${MSYSTEM:-}" in
  Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*) ;;
  *) [ -n "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ] || exit 88 ;;
esac
{
  printf '%s\n' "$#"
  for arg in "$@"; do printf '%s\n' "$arg"; done
} >>"$KIO_TEST_OBSERVER_LOG"
[ "${KIO_TEST_OBSERVER_CRASH:-}" != 1 ] || exit 73
exec "$@"
EOF

cat >"$scratch/observer bin/sccache" <<'EOF'
#!/bin/sh
set -eu
if [ "$#" -eq 1 ] && [ "$1" = --dist-status ]; then
  [ "${SCCACHE_IDLE_TIMEOUT:-}" = 0 ]
  [ -z "${KIO_CI_SCHEDULE_HELD:-}" ]
  [ -z "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ]
  printf '%s\n' "$1" >>"$KIO_TEST_OBSERVER_READINESS_LOG"
  exit 0
fi
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 89 ;;
esac
printf '%s\n' "$@" >>"$KIO_TEST_OBSERVER_WRAPPER_LOG"
exec "$@"
EOF

cat >"$scratch/observer bin/inner-compiler" <<'EOF'
#!/bin/sh
set -eu
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *) exit 90 ;;
esac
case "${OS:-}:${MSYSTEM:-}" in
  Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*) ;;
  *) [ -n "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ] || exit 91 ;;
esac
printf '%s\n' "$@" >"$KIO_TEST_OBSERVER_INNER_LOG"
EOF
chmod +x \
  "$scratch/observer bin/rustc" \
  "$scratch/observer bin/sccache" \
  "$scratch/observer bin/inner-compiler"

test_compiler_observer_readiness_and_lease() {
  tcor_observer=$scratch/'observer bin/rustc'
  tcor_wrapper=$scratch/'observer bin/sccache'
  tcor_inner=$scratch/'observer bin/inner-compiler'
  tcor_state=$scratch/observer-state
  tcor_readiness=$scratch/observer-readiness
  tcor_observer_log=$scratch/observer-argv
  tcor_wrapper_log=$scratch/observer-wrapper-argv
  tcor_inner_log=$scratch/observer-inner-argv
  : >"$tcor_readiness"
  : >"$tcor_observer_log"
  : >"$tcor_wrapper_log"

  KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER=$tcor_observer \
    KIO_TEST_RUNNER_COMPILER_WRAPPER=$tcor_wrapper \
    KIO_TEST_OBSERVER_READINESS_LOG=$tcor_readiness \
    sh "$SCHEDULE_SH" --compiler-readiness -- \
      "$tcor_observer" "$tcor_wrapper" "$tcor_inner"
  [ "$(wc -l <"$tcor_readiness" | tr -d ' ')" -eq 1 ] ||
    fail 'compiler-named observer hid the exact inner readiness wrapper'
  : >"$tcor_readiness"

  # A native Windows scheduler cannot execute this POSIX-script fixture
  # directly. The platform-neutral command-shape units cover the outer argv;
  # the scheduler crate's native Windows tests cover suspended creation and
  # Job assignment for every admitted Command.
  case "${OS:-}:${MSYSTEM:-}" in
    Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*) return ;;
  esac

  # Use the real scheduler here: the outer observer must be the command that
  # receives Unix lease descriptors or Windows Job ownership. Its compiler-like
  # basename is deliberate; exact configured-layer peeling must still discover
  # the inner cache wrapper and run readiness exactly once.
  tcor_scheduler=$(sh "$SCHEDULE_SH" --prepare) ||
    fail 'could not prepare the native scheduler for observer coverage'
  KIO_CI_SCHEDULER_BIN=$tcor_scheduler \
    KIO_CI_SCHEDULE_DIR=$tcor_state \
    KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
    KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER=$tcor_observer \
    KIO_TEST_RUNNER_COMPILER_WRAPPER=$tcor_wrapper \
    KIO_TEST_OBSERVER_READINESS_LOG=$tcor_readiness \
    KIO_TEST_OBSERVER_LOG=$tcor_observer_log \
    KIO_TEST_OBSERVER_WRAPPER_LOG=$tcor_wrapper_log \
    KIO_TEST_OBSERVER_INNER_LOG=$tcor_inner_log \
    sh "$SCHEDULE_SH" --resource compiler -- \
      "$tcor_observer" "$tcor_wrapper" "$tcor_inner" 'argument with spaces'

  [ "$(wc -l <"$tcor_readiness" | tr -d ' ')" -eq 1 ] ||
    fail 'compiler-named observer did not produce exactly one inner readiness probe'
  printf '%s\n' 3 "$tcor_wrapper" "$tcor_inner" 'argument with spaces' \
    >"$scratch/expected-observer-argv"
  assert_same "$scratch/expected-observer-argv" "$tcor_observer_log" \
    'observer did not receive the exact former wrapped compiler command'
  printf '%s\n' "$tcor_inner" 'argument with spaces' \
    >"$scratch/expected-observer-wrapper-argv"
  assert_same "$scratch/expected-observer-wrapper-argv" "$tcor_wrapper_log" \
    'cache wrapper did not receive the exact inner compiler command'
  printf '%s\n' 'argument with spaces' >"$scratch/expected-observer-inner-argv"
  assert_same "$scratch/expected-observer-inner-argv" "$tcor_inner_log" \
    'inner compiler arguments changed across observer and wrapper layers'

  # A crashing outer observer must return its status and release the same
  # process-tree claim; no inner wrapper/compiler invocation may occur.
  if KIO_CI_SCHEDULER_BIN=$tcor_scheduler \
    KIO_CI_SCHEDULE_DIR=$tcor_state \
    KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
    KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER=$tcor_observer \
    KIO_TEST_RUNNER_COMPILER_WRAPPER=$tcor_wrapper \
    KIO_TEST_OBSERVER_CRASH=1 \
    KIO_TEST_OBSERVER_READINESS_LOG=$tcor_readiness \
    KIO_TEST_OBSERVER_LOG=$tcor_observer_log \
    KIO_TEST_OBSERVER_WRAPPER_LOG=$tcor_wrapper_log \
    KIO_TEST_OBSERVER_INNER_LOG=$tcor_inner_log \
    sh "$SCHEDULE_SH" --resource compiler -- \
      "$tcor_observer" "$tcor_wrapper" "$tcor_inner" crash; then
    fail 'crashing compiler observer unexpectedly succeeded'
  else
    tcor_status=$?
    [ "$tcor_status" -eq 73 ] ||
      fail "crashing compiler observer exited $tcor_status rather than 73"
  fi
  [ "$(wc -l <"$tcor_readiness" | tr -d ' ')" -eq 2 ] ||
    fail 'crashing observer command did not run exactly one readiness probe'
  assert_same "$scratch/expected-observer-wrapper-argv" "$tcor_wrapper_log" \
    'crashing compiler observer unexpectedly reached the cache wrapper'
  assert_same "$scratch/expected-observer-inner-argv" "$tcor_inner_log" \
    'crashing compiler observer unexpectedly reached the inner compiler'
  if find "$tcor_state/compiler/claims" -type f -print -quit | grep -q .; then
    fail 'crashing compiler observer left a compiler claim behind'
  fi
}

test_native_dispatch_and_arguments() {
  tnda_state=$scratch/dispatch-state
  mkdir -p "$tnda_state"
  tnda_root=$(native_path "$tnda_state")
  tnda_log=$scratch/dispatch
  tnda_command=$scratch/dispatch-command
  tnda_command_root=$scratch/dispatch-command-root

  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_DIR=$tnda_state \
    KIO_TEST_SCHEDULER_LOG_BASE=$tnda_log \
    KIO_TEST_COMMAND_OUTPUT=$tnda_command \
    KIO_TEST_COMMAND_ROOT=$tnda_command_root \
    sh "$SCHEDULE_SH" -- "$scratch/record-command" \
      alpha 'two words' '' --looks-like-an-option </dev/null

  printf '%s\n' run --resource work --jobs 4 -- \
    "$scratch/record-command" alpha 'two words' '' --looks-like-an-option \
    >"$scratch/expected-work"
  assert_same "$scratch/expected-work" "$tnda_log.work" \
    'default work dispatch changed native arguments'
  printf '%s\n' 4 alpha 'two words' '' --looks-like-an-option \
    >"$scratch/expected-command"
  assert_same "$scratch/expected-command" "$tnda_command" \
    'scheduled command arguments were not forwarded exactly'
  [ "$(cat "$tnda_log.work.root")" = "$tnda_root" ] ||
    fail 'native scheduler did not receive the canonical state root'
  [ "$(cat "$tnda_command_root")" = "$tnda_root" ] ||
    fail 'scheduled command did not inherit the published state root'

  tnda_log=$scratch/barrier
  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_DIR=$tnda_state \
    KIO_CI_SCHEDULE_JOBS=3 \
    KIO_TEST_SCHEDULER_LOG_BASE=$tnda_log \
    sh "$SCHEDULE_SH" --barrier -- true </dev/null
  printf '%s\n' run --resource work --barrier --jobs 3 -- true \
    >"$scratch/expected-barrier"
  assert_same "$scratch/expected-barrier" "$tnda_log.work" \
    'work barrier flag was not forwarded to the native scheduler'

  tnda_log=$scratch/cargo-resource
  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_DIR=$tnda_state \
    KIO_TEST_SCHEDULER_LOG_BASE=$tnda_log \
    sh "$SCHEDULE_SH" --resource cargo -- true </dev/null
  printf '%s\n' run --resource cargo -- true >"$scratch/expected-cargo"
  assert_same "$scratch/expected-cargo" "$tnda_log.cargo" \
    'cargo resource flag was not forwarded to the native scheduler'

  tnda_log=$scratch/compiler-resource
  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_DIR=$tnda_state \
    KIO_TEST_SCHEDULER_LOG_BASE=$tnda_log \
    sh "$SCHEDULE_SH" --resource compiler -- true </dev/null
  printf '%s\n' run --resource compiler -- true \
    >"$scratch/expected-compiler"
  assert_same "$scratch/expected-compiler" "$tnda_log.compiler" \
    'compiler resource without a daemon wrapper gained a readiness hook'

  tnda_log=$scratch/compiler-readiness-resource
  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_DIR=$tnda_state \
    KIO_TEST_SCHEDULER_LOG_BASE=$tnda_log \
    KIO_TEST_RUNNER_COMPILER_WRAPPER=$scratch/bin/sccache \
    sh "$SCHEDULE_SH" --resource compiler -- true </dev/null
  printf '%s\n' run --resource compiler \
    --readiness-hook sh --readiness-hook-arg "$REPO_ROOT/ci/schedule.sh" \
    -- true >"$scratch/expected-compiler-readiness"
  assert_same "$scratch/expected-compiler-readiness" "$tnda_log.compiler" \
    'applicable compiler readiness hook was not forwarded'
}

test_stdin_boundary() {
  tsb_state=$scratch/stdin-state-root
  tsb_log=$scratch/stdin-open
  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_DIR=$tsb_state \
    KIO_TEST_SCHEDULER_LOG_BASE=$tsb_log \
    KIO_TEST_STDIN_OUTPUT=$scratch/stdin-open-result \
    sh "$SCHEDULE_SH" --resource cargo -- "$scratch/stdin-state" </dev/null
  printf '%s\n' run --resource cargo -- "$scratch/stdin-state" \
    >"$scratch/expected-stdin-open"
  assert_same "$scratch/expected-stdin-open" "$tsb_log.cargo" \
    'open stdin was reported as closed'
  [ "$(cat "$scratch/stdin-open-result")" = open ] ||
    fail 'open stdin was not preserved for the command'

  tsb_log=$scratch/stdin-closed
  (
    export KIO_CI_SCHEDULER_BIN="$scratch/fake-scheduler"
    export KIO_CI_SCHEDULE_DIR="$tsb_state"
    export KIO_TEST_SCHEDULER_LOG_BASE="$tsb_log"
    export KIO_TEST_STDIN_OUTPUT="$scratch/stdin-closed-result"
    exec 0<&-
    exec sh "$SCHEDULE_SH" --resource cargo -- "$scratch/stdin-state"
  )
  printf '%s\n' run --resource cargo --stdin-closed -- "$scratch/stdin-state" \
    >"$scratch/expected-stdin-closed"
  assert_same "$scratch/expected-stdin-closed" "$tsb_log.cargo" \
    'closed stdin was not reported to the native scheduler'
  [ "$(cat "$scratch/stdin-closed-result")" = closed ] ||
    fail 'closed stdin was reopened for the command'
}

test_supervision_boundary() {
  tsb_drained=$scratch/control/drained
  tsb_cancel=$scratch/control/cancel
  mkdir -p "$scratch/control"

  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_TEST_SCHEDULER_LOG_BASE=$scratch/supervision-open \
    sh "$SCHEDULE_SH" --supervise \
      --drained-marker "$tsb_drained" --cancel-file "$tsb_cancel" -- true \
      </dev/null
  printf '%s\n' supervise \
    --drained-marker "$tsb_drained" --cancel-file "$tsb_cancel" -- true \
    >"$scratch/expected-supervision-open"
  assert_same "$scratch/expected-supervision-open" \
    "$scratch/supervision-open.supervise" \
    'supervision arguments changed at the shell facade'

  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_TEST_SCHEDULER_LOG_BASE=$scratch/supervision-closed \
    sh -c 'exec 0<&-; exec sh "$@"' sh "$SCHEDULE_SH" --supervise \
    --drained-marker "$tsb_drained" --cancel-file "$tsb_cancel" -- true
  printf '%s\n' supervise --stdin-closed \
    --drained-marker "$tsb_drained" --cancel-file "$tsb_cancel" -- true \
    >"$scratch/expected-supervision-closed"
  assert_same "$scratch/expected-supervision-closed" \
    "$scratch/supervision-closed.supervise" \
    'closed stdin was not forwarded through supervision'

  cat >"$scratch/bin/cygpath" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = -m ] || exit 92
case "$2" in
  "$KIO_TEST_WINDOWS_DRAINED") printf '%s\n' 'C:/fixture/drained' ;;
  "$KIO_TEST_WINDOWS_CANCEL") printf '%s\n' 'C:/fixture/cancel' ;;
  *) exit 92 ;;
esac
EOF
  chmod +x "$scratch/bin/cygpath"
  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_TEST_SCHEDULER_LOG_BASE=$scratch/supervision-windows \
    KIO_TEST_WINDOWS_DRAINED=$tsb_drained \
    KIO_TEST_WINDOWS_CANCEL=$tsb_cancel \
    OS=Windows_NT MSYSTEM='' PATH="$scratch/bin:$PATH" \
    sh "$SCHEDULE_SH" --supervise \
      --drained-marker "$tsb_drained" --cancel-file "$tsb_cancel" -- true \
      </dev/null
  printf '%s\n' supervise \
    --drained-marker 'C:/fixture/drained' \
    --cancel-file 'C:/fixture/cancel' -- true \
    >"$scratch/expected-supervision-windows"
  assert_same "$scratch/expected-supervision-windows" \
    "$scratch/supervision-windows.supervise" \
    'Windows supervision paths were not converted explicitly'
}

test_state_discovery_and_publication() {
  tsdap_common=$scratch/git-common
  mkdir -p "$tsdap_common"
  tsdap_log=$scratch/discovered
  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_JOBS=2 \
    KIO_TEST_SCHEDULER_LOG_BASE=$tsdap_log \
    KIO_TEST_GIT_COMMON=$tsdap_common \
    KIO_TEST_COMMAND_OUTPUT=$scratch/discovered-command \
    KIO_TEST_COMMAND_ROOT=$scratch/discovered-command-root \
    PATH="$scratch/bin:$PATH" \
    sh "$SCHEDULE_SH" -- "$scratch/record-command" </dev/null
  tsdap_expected=$(native_path "$tsdap_common/kio-ci-schedule")
  [ "$(cat "$tsdap_log.work.root")" = "$tsdap_expected" ] ||
    fail 'Git-common state discovery did not reach the native scheduler'
  [ "$(cat "$scratch/discovered-command-root")" = "$tsdap_expected" ] ||
    fail 'Git-common state discovery was not inherited by the command'
  [ -d "$tsdap_common/kio-ci-schedule" ] ||
    fail 'Git-common state directory was not created'

  mkdir -p "$scratch/caller" "$scratch/new-cwd"
  tsdap_log=$scratch/relative
  (
    cd "$scratch/caller"
    KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
      KIO_CI_SCHEDULE_DIR=relative-state \
      KIO_CI_SCHEDULE_JOBS=2 \
      KIO_TEST_SCHEDULER_LOG_BASE=$tsdap_log \
      KIO_TEST_COMMAND_OUTPUT=$scratch/relative-command \
      KIO_TEST_COMMAND_ROOT=$scratch/relative-command-root \
      sh "$SCHEDULE_SH" -- sh -c \
        'cd "$1" && exec "$2"' sh "$scratch/new-cwd" \
        "$scratch/record-command" </dev/null
  )
  tsdap_expected=$(native_path "$scratch/caller/relative-state")
  [ "$(cat "$tsdap_log.work.root")" = "$tsdap_expected" ] ||
    fail 'relative explicit state root was not canonicalized before dispatch'
  [ "$(cat "$scratch/relative-command-root")" = "$tsdap_expected" ] ||
    fail 'a command cwd change altered the published state root'
}

test_windows_state_bridge() {
  twsb_shell_root=$scratch/windows-shell-state
  cat >"$scratch/bin/cygpath" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}" in
  -u)
    [ "$2" = 'C:\fixture\state' ] || exit 92
    printf '%s\n' "$KIO_TEST_WINDOWS_SHELL_ROOT"
    ;;
  -m)
    [ "$2" = "$KIO_TEST_WINDOWS_SHELL_ROOT" ] || exit 92
    printf '%s\n' 'C:/fixture/state'
    ;;
  *) exit 92 ;;
esac
EOF
  chmod +x "$scratch/bin/cygpath"

  KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
    KIO_CI_SCHEDULE_DIR='C:\fixture\state' \
    KIO_CI_SCHEDULE_JOBS=2 \
    KIO_TEST_SCHEDULER_LOG_BASE=$scratch/windows \
    KIO_TEST_WINDOWS_SHELL_ROOT=$twsb_shell_root \
    OS=Windows_NT MSYSTEM='' \
    PATH="$scratch/bin:$PATH" \
    sh "$SCHEDULE_SH" -- true </dev/null
  [ "$(cat "$scratch/windows.work.root")" = 'C:/fixture/state' ] ||
    fail 'Windows shell path was not converted for the native scheduler'
  [ -d "$twsb_shell_root" ] ||
    fail 'Windows shell form of the state directory was not created'
}

test_explicit_bypass() {
  teb_marker=$scratch/unexpected-scheduler
  cat >"$scratch/fail-scheduler" <<'EOF'
#!/bin/sh
: >"$KIO_TEST_UNEXPECTED_SCHEDULER"
exit 99
EOF
  chmod +x "$scratch/fail-scheduler"

  for teb_resource in work cargo compiler; do
    teb_output=$scratch/bypass-$teb_resource
    KIO_CI_SCHEDULE=DISABLE \
      KIO_CI_SCHEDULER_BIN=$scratch/fail-scheduler \
      KIO_CI_SCHEDULE_DIR=$scratch/uncreated-$teb_resource \
      KIO_TEST_UNEXPECTED_SCHEDULER=$teb_marker \
      KIO_TEST_COMMAND_OUTPUT=$teb_output \
      KIO_TEST_COMMAND_ROOT=$scratch/bypass-root-$teb_resource \
      sh "$SCHEDULE_SH" --resource "$teb_resource" -- \
        "$scratch/record-command" bypass "$teb_resource" </dev/null
    printf '%s\n' 2 bypass "$teb_resource" >"$scratch/expected-bypass"
    assert_same "$scratch/expected-bypass" "$teb_output" \
      "$teb_resource bypass changed command arguments"
    [ ! -e "$scratch/uncreated-$teb_resource" ] ||
      fail "$teb_resource bypass created scheduler state"
  done
  [ ! -e "$teb_marker" ] || fail 'explicit bypass invoked the native scheduler'

  : >"$scratch/inherited-bypass-sccache-log"
  KIO_CI_SCHEDULE=DISABLE \
    KIO_CI_SCHEDULE_HELD=compiler \
    KIO_CI_SCHEDULER_BIN=$scratch/fail-scheduler \
    KIO_TEST_RUNNER_COMPILER_WRAPPER=$scratch/bin/sccache \
    KIO_TEST_SCCACHE_LOG=$scratch/inherited-bypass-sccache-log \
    KIO_TEST_COMMAND_OUTPUT=$scratch/inherited-bypass-output \
    KIO_TEST_COMMAND_ROOT=$scratch/inherited-bypass-root \
    sh "$SCHEDULE_SH" --resource compiler -- \
      "$scratch/record-command" inherited bypass </dev/null
  [ ! -s "$scratch/inherited-bypass-sccache-log" ] ||
    fail 'inherited compiler bypass repeated established readiness'
}

test_worker_marker_validation() {
  twmv_marker=$scratch/invalid-worker-marker-routed
  cat >"$scratch/stop-worker-scheduler" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}" in
  available-parallelism) printf '%s\n' 1 ;;
  run) : >"$KIO_TEST_INVALID_WORKER_ROUTED"; exit 73 ;;
  *) exit 95 ;;
esac
EOF
  chmod +x "$scratch/stop-worker-scheduler"

  set +e
  KIO_CI_SCHEDULE_HELD=work,work \
    KIO_CI_SCHEDULER_BIN=$scratch/stop-worker-scheduler \
    KIO_CI_SCHEDULE_DIR=$scratch/invalid-worker-state \
    KIO_TEST_INVALID_WORKER_ROUTED=$twmv_marker \
    sh "$REPO_ROOT/ci/run-tests.sh" --__worker invalid-marker \
      >/dev/null 2>&1
  twmv_status=$?
  set -e
  if [ "$twmv_status" -ne 73 ] || [ ! -e "$twmv_marker" ]; then
    fail 'noncanonical inherited work marker bypassed native validation'
  fi
}

test_cargo_facade() {
  tcf_cwd=$scratch/cargo-cwd
  tcf_state=$scratch/cargo-state
  mkdir -p "$tcf_cwd"

  tcf_log=$scratch/standalone-cargo
  (
    cd "$tcf_cwd"
    PATH="$scratch/bin:$PATH" \
      KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
      KIO_CI_SCHEDULE_DIR=$tcf_state \
      KIO_TEST_SCHEDULER_LOG_BASE=$tcf_log \
      KIO_TEST_CARGO_OUTPUT=$scratch/standalone-cargo-output \
      sh "$CARGO_SH" check 'argument with spaces' </dev/null
  )
  printf '%s\n' run --resource compiler \
    -- cargo check 'argument with spaces' >"$scratch/expected-cargo-wrapper"
  assert_same "$scratch/expected-cargo-wrapper" "$tcf_log.compiler" \
    'standalone Cargo did not request only the compiler resource'
  [ ! -e "$tcf_log.cargo" ] ||
    fail 'standalone Cargo unexpectedly requested the optional cargo resource'
  printf '%s\n' "$tcf_cwd" 2 check 'argument with spaces' compiler \
    >"$scratch/expected-cargo-command"
  assert_same "$scratch/expected-cargo-command" \
    "$scratch/standalone-cargo-output" \
    'Cargo cwd, arguments, or compiler marker were not preserved'

  tcf_log=$scratch/serialized-cargo
  (
    cd "$tcf_cwd"
    PATH="$scratch/bin:$PATH" \
      KIO_CI_SERIALIZE_CARGO=1 \
      KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler \
      KIO_CI_SCHEDULE_DIR=$tcf_state \
      KIO_TEST_SCHEDULER_LOG_BASE=$tcf_log \
      KIO_TEST_CARGO_OUTPUT=$scratch/serialized-cargo-output \
      sh "$CARGO_SH" test --locked </dev/null
  )
  printf '%s\n' run --resource cargo -- sh "$CARGO_SH" test --locked \
    >"$scratch/expected-outer-cargo"
  assert_same "$scratch/expected-outer-cargo" "$tcf_log.cargo" \
    'serialized Cargo did not request the cargo resource first'
  printf '%s\n' run --resource compiler \
    -- cargo test --locked >"$scratch/expected-inner-compiler"
  assert_same "$scratch/expected-inner-compiler" "$tcf_log.compiler" \
    'serialized Cargo did not request compiler after cargo admission'
  printf '%s\n' "$tcf_cwd" 2 test --locked cargo,compiler \
    >"$scratch/expected-serialized-command"
  assert_same "$scratch/expected-serialized-command" \
    "$scratch/serialized-cargo-output" \
    'serialized Cargo did not preserve cwd, arguments, or resource order'

  rm -f "$scratch/unexpected-scheduler"
  KIO_CI_SCHEDULE=DISABLE \
    KIO_CI_SCHEDULER_BIN=$scratch/fail-scheduler \
    KIO_TEST_UNEXPECTED_SCHEDULER=$scratch/unexpected-scheduler \
    KIO_TEST_CARGO_OUTPUT=$scratch/bypassed-cargo-output \
    PATH="$scratch/bin:$PATH" \
    sh "$CARGO_SH" metadata --no-deps </dev/null
  [ ! -e "$scratch/unexpected-scheduler" ] ||
    fail 'Cargo explicit bypass invoked the native scheduler'

  : >"$scratch/cargo-bypass-sccache-log"
  KIO_CI_SCHEDULE=DISABLE \
    RUSTC_WRAPPER=$scratch/bin/sccache \
    KIO_TEST_SCCACHE_LOG=$scratch/cargo-bypass-sccache-log \
    KIO_TEST_CARGO_OUTPUT=$scratch/bypassed-cargo-sccache-output \
    PATH="$scratch/bin:$PATH" \
    sh "$CARGO_SH" metadata --no-deps </dev/null
  [ "$(cat "$scratch/cargo-bypass-sccache-log")" = --dist-status ] ||
    fail 'Cargo explicit bypass skipped isolated sccache readiness'
}

test_stable_corpus_tool_target() {
  tsct_state=$scratch/stable-corpus-tool-state
  tsct_workspace=$scratch/'corpus tool workspace'
  tsct_build_log=$scratch/stable-corpus-tool-builds
  tsct_invocation_log=$scratch/stable-corpus-tool-invocations
  tsct_copy_log=$scratch/stable-corpus-tool-copies
  tsct_real_cp=$(command -v cp)
  tsct_path=$scratch/bin:$PATH
  mkdir -p "$tsct_state" "$tsct_workspace"
  : >"$tsct_build_log"
  : >"$tsct_invocation_log"
  : >"$tsct_copy_log"

  # Model Cargo fingerprints: every invocation is visible, but only the first
  # call for one target produces the executable. The native scheduler's
  # capacity-one behavior is covered by its Rust tests; this fixture pins the
  # shell helper's target identity, resource order, and private-copy boundary.
  cat >"$scratch/bin/cargo" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = build ] || exit 91
shift
target=
binary=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --target-dir)
      [ "$#" -ge 2 ] || exit 92
      target=$2
      shift 2
      ;;
    --bin)
      [ "$#" -ge 2 ] || exit 92
      binary=$2
      shift 2
      ;;
    *) shift ;;
  esac
done
[ -n "$target" ] && [ -n "$binary" ] || exit 92
printf '%s\t%s\n' "${KIO_CI_SCHEDULE_HELD:-}" "$target" \
  >>"$KIO_TEST_CORPUS_TOOL_INVOCATION_LOG"
if [ ! -f "$target/debug/$binary" ]; then
  printf '%s\n' "$target" >>"$KIO_TEST_CORPUS_TOOL_BUILD_LOG"
  mkdir -p "$target/debug"
  printf 'fixture corpus tool\n' >"$target/debug/$binary"
fi
EOF
  chmod +x "$scratch/bin/cargo"

  cat >"$scratch/bin/cp" <<'EOF'
#!/bin/sh
set -eu
printf '%s\t%s\t%s\n' "${KIO_CI_SCHEDULE_HELD:-}" "$1" "$2" \
  >>"$KIO_TEST_CORPUS_TOOL_COPY_LOG"
exec "$KIO_TEST_REAL_CP" "$@"
EOF
  chmod +x "$scratch/bin/cp"

  tsct_call() {
    tsct_call_tmp=$1
    tsct_call_held=$2
    tsct_call_schedule=$3
    tsct_call_key=$4
    tsct_call_workspace=$5
    tsct_call_destination=$6
    shift 6
    (
      ORCHESTRATOR_TMP=$tsct_call_tmp
      KIO_CI_SCHEDULE_HELD=$tsct_call_held
      KIO_CI_SCHEDULE=$tsct_call_schedule
      KIO_CI_SCHEDULER_BIN=$scratch/fake-scheduler
      KIO_CI_SCHEDULE_DIR=$tsct_state
      KIO_TEST_SCHEDULER_LOG_BASE=$tsct_call_destination
      KIO_TEST_CORPUS_TOOL_BUILD_LOG=$tsct_build_log
      KIO_TEST_CORPUS_TOOL_INVOCATION_LOG=$tsct_invocation_log
      KIO_TEST_CORPUS_TOOL_COPY_LOG=$tsct_copy_log
      KIO_TEST_REAL_CP=$tsct_real_cp
      export \
        ORCHESTRATOR_TMP \
        KIO_CI_SCHEDULE_HELD \
        KIO_CI_SCHEDULE \
        KIO_CI_SCHEDULER_BIN \
        KIO_CI_SCHEDULE_DIR \
        KIO_TEST_SCHEDULER_LOG_BASE \
        KIO_TEST_CORPUS_TOOL_BUILD_LOG \
        KIO_TEST_CORPUS_TOOL_INVOCATION_LOG \
        KIO_TEST_CORPUS_TOOL_COPY_LOG \
        KIO_TEST_REAL_CP
      PATH=$tsct_path
      export PATH
      # shellcheck disable=SC1090 # Exercise the production helper by path.
      . "$ORCHESTRATOR_COMMON"
      build_corpus_tool_binary \
        "$tsct_call_key" "$tsct_call_workspace" fixture-tool \
        "$tsct_call_destination" "$@"
    )
  }

  # Reject path traversal and relative workspaces before the helper can ask
  # either the scheduler or Cargo to touch a target.
  tsct_validation_private=$scratch/'corpus validation private'
  tsct_validation_destination=$tsct_validation_private/fixture-tool
  mkdir -p "$tsct_validation_private"
  for tsct_bad_key in . ..; do
    if tsct_call \
      "$tsct_validation_private" '' '' "$tsct_bad_key" \
      "$tsct_workspace" "$tsct_validation_destination" \
      --bin fixture-tool \
      >"$scratch/invalid-key.out" 2>"$scratch/invalid-key.err"; then
      fail "corpus-tool helper accepted invalid key $tsct_bad_key"
    else
      tsct_status=$?
      [ "$tsct_status" -eq 2 ] ||
        fail "invalid corpus-tool key exited $tsct_status rather than 2"
    fi
  done
  if tsct_call \
    "$tsct_validation_private" '' '' fixture-tool 'relative workspace' \
    "$tsct_validation_destination" --bin fixture-tool \
    >"$scratch/relative-workspace.out" \
    2>"$scratch/relative-workspace.err"; then
    fail 'corpus-tool helper accepted a relative workspace'
  else
    tsct_status=$?
    [ "$tsct_status" -eq 2 ] ||
      fail "relative corpus-tool workspace exited $tsct_status rather than 2"
  fi
  if [ -s "$tsct_build_log" ] || [ -s "$tsct_invocation_log" ] ||
    [ -s "$tsct_copy_log" ]; then
    fail 'invalid corpus-tool input reached Cargo or the private copy'
  fi
  [ ! -e "$tsct_validation_destination.cargo" ] ||
    fail 'invalid corpus-tool input reached scheduler admission'

  tsct_i=1
  while [ "$tsct_i" -le 4 ]; do
    tsct_private=$scratch/'corpus private '"$tsct_i"
    tsct_destination=$tsct_private/fixture-tool
    tsct_held=
    [ "$tsct_i" -gt 2 ] || tsct_held=work
    mkdir -p "$tsct_private"
    tsct_call \
      "$tsct_private" "$tsct_held" '' fixture-tool "$tsct_workspace" \
      "$tsct_destination" --bin fixture-tool
    [ "$(cat "$tsct_destination")" = 'fixture corpus tool' ] ||
      fail "corpus-tool caller $tsct_i did not receive its private copy"
    [ "$(tail -n 1 "$tsct_copy_log" | cut -f3)" = "$tsct_destination" ] ||
      fail "corpus-tool caller $tsct_i copy escaped its private destination"
    [ -f "$tsct_destination.cargo" ] ||
      fail "corpus-tool caller $tsct_i did not request cargo admission"
    tsct_i=$((tsct_i + 1))
  done

  tsct_target=$tsct_workspace/target/kio-corpus-tools/fixture-tool
  [ "$(cat "$tsct_build_log")" = "$tsct_target" ] ||
    fail 'four corpus-tool callers did not converge on one target build'
  [ "$(wc -l <"$tsct_invocation_log" | tr -d ' ')" -eq 4 ] ||
    fail 'stable corpus-tool target did not receive four Cargo invocations'
  if ! awk -F '\t' -v target="$tsct_target" '
      NR <= 2 && ($1 != "work,cargo,compiler" || $2 != target) { bad=1 }
      NR > 2 && ($1 != "cargo,compiler" || $2 != target) { bad=1 }
      END { exit bad }
    ' "$tsct_invocation_log"; then
    fail 'corpus-tool build changed target identity or cargo -> compiler order'
  fi
  if ! awk -F '\t' -v target="$tsct_target/debug/fixture-tool" '
      NR <= 2 && ($1 != "work,cargo" || $2 != target) { bad=1 }
      NR > 2 && ($1 != "cargo" || $2 != target) { bad=1 }
      END { exit NR != 4 || bad }
    ' "$tsct_copy_log"; then
    fail 'corpus-tool private copy escaped cargo admission'
  fi

  # The explicit scheduler bypass has no cargo lease, so it must retain the
  # invocation-private target rather than touching the persistent one.
  tsct_bypass_i=1
  while [ "$tsct_bypass_i" -le 2 ]; do
    tsct_bypass=$scratch/'bypassed corpus private '"$tsct_bypass_i"
    mkdir -p "$tsct_bypass"
    tsct_call \
      "$tsct_bypass" '' DISABLE fixture-tool "$tsct_workspace" \
      "$tsct_bypass/fixture-tool" --bin fixture-tool
    [ -f "$tsct_bypass/fixture-tool-target/debug/fixture-tool" ] ||
      fail 'scheduler bypass did not retain its private corpus-tool target'
    [ "$(cat "$tsct_bypass/fixture-tool")" = 'fixture corpus tool' ] ||
      fail 'scheduler bypass did not receive its private copy'
    grep -Fxq "$tsct_bypass/fixture-tool-target" "$tsct_build_log" ||
      fail 'scheduler bypass mutated the persistent corpus-tool target'
    tsct_bypass_i=$((tsct_bypass_i + 1))
  done
  [ "$(wc -l <"$tsct_build_log" | tr -d ' ')" -eq 3 ] ||
    fail 'scheduler bypass invocations unexpectedly shared a target'
  if ! tail -n 2 "$tsct_copy_log" | awk -F '\t' '
      $1 != "" { bad=1 }
      END { exit NR != 2 || bad }
    '; then
    fail 'scheduler bypass unexpectedly retained cargo admission'
  fi
}

test_windows_smoke_compiler_admission() {
  twsca_block=$scratch/windows-compiler-smoke
  awk '
    /^      - name: Compiler smoke / { capture=1 }
    capture && seen && /^      - name: / { exit }
    capture { print; seen=1 }
  ' "$REPO_ROOT/.github/workflows/ci.yml" >"$twsca_block"

  [ -s "$twsca_block" ] ||
    fail 'Windows compiler-smoke workflow block is missing'
  for twsca_command in check test build; do
    twsca_count=$(grep -Fxc \
      "            --resource compiler -- \"\$kio\" $twsca_command" \
      "$twsca_block" || :)
    [ "$twsca_count" -eq 1 ] ||
      fail "Windows $twsca_command smoke does not request one compiler permit"
  done
  if grep -Eq '^          "[^" ]+" (check|test|build)$' "$twsca_block"; then
    fail 'Windows compiler smoke contains a direct compiler-producing Kio call'
  fi
}

test_native_dispatch_and_arguments
test_compiler_observer_readiness_and_lease
test_stdin_boundary
test_supervision_boundary
test_state_discovery_and_publication
test_windows_state_bridge
test_explicit_bypass
test_worker_marker_validation
test_cargo_facade
test_stable_corpus_tool_target
test_windows_smoke_compiler_admission
printf 'schedule-entry-selftest: ok\n'
