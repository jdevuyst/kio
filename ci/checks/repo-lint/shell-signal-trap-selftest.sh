#!/bin/sh

# Keep the primary corpus harnesses' cleanup and catchable-signal termination
# separate. A signal handler that only removes scratch state returns to the
# interrupted script; EXIT owns cleanup, while HUP/INT/TERM terminate.
# shellcheck disable=SC2016 # Static inventory strings intentionally stay literal.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
SELFTEST=$SCRIPT_DIR/$(basename -- "$0")

if [ "${1:-}" = --signal-parent ]; then
  [ "$#" -eq 4 ] || exit 2
  signal_pid=$2
  signal_name=$3
  signal_done=$4
  case "$signal_name" in HUP|INT|TERM) ;; *) exit 2 ;; esac
  kill -s "$signal_name" "$signal_pid"
  printf 'helper-done\n' >"$signal_done"
  exit 0
fi

if [ "${1:-}" = --script-command ]; then
  [ "$#" -eq 5 ] || exit 2
  command_parent=$2 command_signal=$3 command_result=$4 command_status=$5
  printf '%s\n' "$$" >"$command_result/child-pid"
  printf 'child-start\n' >>"$command_result/events"
  case "$command_signal" in
    NONE) ;;
    HUP|INT|TERM)
      kill -s "$command_signal" "$command_parent"
      sleep 1
      ;;
    *) exit 2 ;;
  esac
  IFS= read -r command_tree <"$command_result/tree-path"
  [ -d "$command_tree" ] || exit 97
  printf 'child-complete\n' >>"$command_result/events"
  exit "$command_status"
fi

scratch_parent=${TMPDIR:-$REPO_ROOT/target}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/shell-signal-trap.XXXXXX")

# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_selftest() {
  cleanup_selftest_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_selftest_status"
}
trap cleanup_selftest EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fail() {
  printf 'shell-signal-trap-selftest: %s\n' "$*" >&2
  exit 1
}

run_fixture() {
  fixture_mode=$1
  fixture_signal=$2
  fixture_dir=$scratch/fixture-$fixture_mode-$fixture_signal
  fixture_tree=$fixture_dir/tree
  fixture_events=$fixture_dir/events
  fixture_resumed=$fixture_dir/resumed
  fixture_helper_done=$fixture_dir/helper-done
  fixture_second_done=$fixture_dir/second-helper-done
  mkdir -p "$fixture_tree"
  : >"$fixture_events"

  set +e
  { sh -c '
    mode=$1
    signal_name=$2
    tree=$3
    events=$4
    resumed=$5
    helper_done=$6
    selftest=$7
    second_done=$8
    handled=

    cleanup_fixed() {
      cleanup_status=$?
      trap "" HUP INT TERM
      if [ "$mode" = fixed-second ]; then
        sh "$selftest" --signal-parent "$$" TERM "$second_done"
      fi
      trap - EXIT
      rm -rf "$tree"
      [ -z "$handled" ] || printf "handled:%s\n" "$handled" >>"$events"
      printf "cleanup\n" >>"$events"
      exit "$cleanup_status"
    }

    case "$mode" in
      fixed|fixed-second|normal)
        trap cleanup_fixed EXIT
        trap "handled=INT; exit 130" INT
        trap "handled=TERM; exit 143" TERM
        trap "handled=HUP; exit 129" HUP
        ;;
      recombined)
        trap '\''rm -rf "$tree"; printf "cleanup\\n" >>"$events"'\'' \
          EXIT HUP INT TERM
        ;;
      missing-hup)
        trap cleanup_fixed EXIT
        trap "handled=INT; exit 130" INT
        trap "handled=TERM; exit 143" TERM
        ;;
      swallowed-term)
        trap cleanup_fixed EXIT
        trap "handled=INT; exit 130" INT
        trap : TERM
        trap "handled=HUP; exit 129" HUP
        ;;
      *) exit 2 ;;
    esac

    [ "$mode" != normal ] || exit 17
    sh "$selftest" --signal-parent "$$" "$signal_name" "$helper_done"
    printf "resumed\n" >"$resumed"
  ' sh "$fixture_mode" "$fixture_signal" "$fixture_tree" "$fixture_events" \
    "$fixture_resumed" "$fixture_helper_done" "$SELFTEST" \
    "$fixture_second_done"; } \
    >"$fixture_dir/stdout" 2>"$fixture_dir/stderr"
  fixture_status=$?
  set -e
}

fixed_result_is_valid() {
  expected_status=$1
  [ "$fixture_status" -eq "$expected_status" ] &&
    [ ! -e "$fixture_tree" ] &&
    [ ! -e "$fixture_resumed" ] &&
    [ -f "$fixture_helper_done" ] &&
    [ "$(grep -c "^handled:$fixture_signal$" "$fixture_events" || :)" -eq 1 ] &&
    [ "$(grep -c '^cleanup$' "$fixture_events" || :)" -eq 1 ]
}

int_probe=$scratch/int-probe
int_resumed=$scratch/int-resumed
set +e
sh -c '
  trap '\''printf "trapped\n" >"$1"; exit 130'\'' INT
  kill -s INT "$$"
  printf "resumed\n" >"$2"
  exit 99
' sh "$int_probe" "$int_resumed" >/dev/null 2>&1
int_probe_status=$?
set -e
if [ "$int_probe_status" -eq 130 ] && [ -f "$int_probe" ] &&
   [ ! -e "$int_resumed" ]; then
  int_trappable=1
elif [ "$int_probe_status" -eq 99 ] && [ -f "$int_resumed" ]; then
  int_trappable=0
else
  fail "cannot classify inherited INT disposition (status $int_probe_status)"
fi
if [ "$int_trappable" -eq 0 ] &&
   [ "${KIO_TEST_REQUIRE_TRAPPABLE_INT:-0}" = 1 ]; then
  fail 'INT is inherited ignored on a route that requires direct signal evidence'
fi

signal_cases='HUP:129 TERM:143'
[ "$int_trappable" -eq 0 ] || signal_cases='HUP:129 INT:130 TERM:143'
for signal_case in $signal_cases; do
  signal_name=${signal_case%%:*}
  signal_status=${signal_case#*:}
  run_fixture fixed "$signal_name"
  fixed_result_is_valid "$signal_status" ||
    fail "fixed $signal_name fixture did not exit $signal_status and clean once"
done

run_fixture fixed-second HUP
if ! fixed_result_is_valid 129 || [ ! -f "$fixture_second_done" ]; then
  fail 'second catchable signal disturbed cleanup or its entering status'
fi

run_fixture normal NONE
if [ "$fixture_status" -ne 17 ] || [ -e "$fixture_tree" ] ||
   [ -e "$fixture_resumed" ] || [ -e "$fixture_helper_done" ] ||
   [ "$(grep -c '^cleanup$' "$fixture_events" || :)" -ne 1 ]; then
  fail 'normal nonzero exit was not preserved through one cleanup'
fi

# A cleanup-only signal trap returns to the interrupted script; keep that causal
# counterexample alongside missing-signal and swallowed-termination mutants.
run_fixture recombined TERM
if [ "$fixture_status" -ne 0 ] || [ ! -f "$fixture_resumed" ] ||
   [ "$(grep -c '^cleanup$' "$fixture_events" || :)" -ne 2 ]; then
  fail 'recombined cleanup fixture did not demonstrate post-signal resumption'
fi

# Bash can run EXIT cleanup even for an unhandled HUP. Require evidence that
# the explicit signal handler ran, not just its status and cleanup effects.
run_fixture missing-hup HUP
if fixed_result_is_valid 129; then
  fail 'missing-HUP mutant survived'
fi

run_fixture swallowed-term TERM
if fixed_result_is_valid 143; then
  fail 'swallowed-TERM mutant survived'
fi

run_script_fixture() {
  script_kind=$1 script_path=$2 script_mode=$3 script_normal_status=$4
  script_result=$scratch/script-$script_kind-$script_mode
  mkdir -p "$script_result/tmp"
  : >"$script_result/events"
  set +e
  (
    cd "$(dirname "$REPO_ROOT/$script_path")" || exit 2
    TMPDIR="$script_result/tmp" KIO_TMP_DIR="$script_result/tmp" \
      sh -c '
      probe_result=$1 probe_mode=$2 probe_normal_status=$3 probe_kind=$4 probe_selftest=$5
      probe_signal=$probe_mode
      [ "$probe_mode" != second ] || probe_signal=HUP
      mktemp() {
        probe_tree=$(command mktemp "$@") || return
        printf "%s\n" "$probe_tree" >"$probe_result/tree-path"
        printf "%s\n" "$probe_tree"
      }
      cp() {
        if [ "$probe_kind" = surface ]; then
          command cp "$@"
        elif [ "$1" = -R ]; then
          mkdir -p "$3"
        fi
      }
      probe_final_command() {
        command sh "$probe_selftest" --script-command "$$" \
          "$probe_signal" "$probe_result" "$probe_normal_status"
      }
      probe_kio() {
        [ "$1" != dep ] || { probe_final_command; return $?; }
      }
      probe_runner() { probe_final_command; }
      grep() {
        command grep "$@" || return $?
        if [ "$probe_kind" = surface ] && [ "${label:-}" = restored-predicates ]; then
          probe_final_command
        fi
      }
      rm() {
        IFS= read -r probe_tree <"$probe_result/tree-path"
        if [ "$#" -eq 2 ] && [ "$1" = -rf ] && [ "$2" = "$probe_tree" ]; then
          printf "cleanup\n" >>"$probe_result/events"
          if [ "$probe_mode" = second ] && [ ! -f "$probe_result/second-sent" ]; then
            : >"$probe_result/second-sent"
            command sh "$probe_selftest" --signal-parent "$$" TERM "$probe_result/second-done"
          fi
        fi
        command rm "$@"
      }
      KIO_BIN=probe_kio KIO_RUNNER=probe_runner KIO_TARGET=js
      . "$0"
      printf "resumed\n" >"$probe_result/resumed"
    ' "$REPO_ROOT/$script_path" "$script_result" "$script_mode" \
      "$script_normal_status" "$script_kind" "$SELFTEST"
  ) >"$script_result/stdout" 2>"$script_result/stderr"
  script_status=$?
  set -e
}

script_failures=0
for script_spec in \
  surface:ci/checks/repo-lint/dyn-load-host-surface-selftest.sh:0 \
  binding:test-data/goldens/00_success/exec_dyn_load_binding_origins/run.sh:0 \
  duplicate:test-data/goldens/30_dependency_error/dep_retype_duplicate_selector/run.sh:30 \
  conflicting:test-data/goldens/30_dependency_error/dep_retype_conflicting_overlap/run.sh:30
do
  script_kind=${script_spec%%:*}
  script_path=${script_spec#*:}
  script_normal_status=${script_path##*:}
  script_path=${script_path%:*}
  for script_case in "NONE:$script_normal_status" $signal_cases second:129; do
    script_mode=${script_case%%:*}
    script_expected_status=${script_case#*:}
    run_script_fixture "$script_kind" "$script_path" "$script_mode" "$script_normal_status"
    printf 'child-start\nchild-complete\ncleanup\n' >"$script_result/expected.events"
    script_valid=1
    [ "$script_status" -eq "$script_expected_status" ] || script_valid=0
    cmp -s "$script_result/expected.events" "$script_result/events" || script_valid=0
    if [ -f "$script_result/tree-path" ]; then
      IFS= read -r script_tree <"$script_result/tree-path"
      [ ! -e "$script_tree" ] || script_valid=0
    else
      script_valid=0
    fi
    if [ -f "$script_result/child-pid" ]; then
      IFS= read -r script_child <"$script_result/child-pid"
      if kill -0 "$script_child" 2>/dev/null; then script_valid=0; fi
    else
      script_valid=0
    fi
    if [ "$script_mode" != NONE ]; then
      [ ! -e "$script_result/resumed" ] || script_valid=0
      if grep -Fq 'dyn-load-host-surface-selftest: ok' "$script_result/stdout"; then
        script_valid=0
      fi
    fi
    if [ "$script_mode" = second ] && [ ! -f "$script_result/second-done" ]; then
      script_valid=0
    fi
    if [ "$script_valid" -ne 1 ]; then
      printf 'shell-signal-trap-selftest: %s %s: status %s, expected %s; events:\n' \
        "$script_kind" "$script_mode" "$script_status" "$script_expected_status" >&2
      cat "$script_result/events" "$script_result/stderr" >&2
      script_failures=$((script_failures + 1))
    fi
  done
done
[ "$script_failures" -eq 0 ] || fail "$script_failures actual-script cancellation probes failed"

expect_line_once() {
  elo_file=$1
  elo_line=$2
  elo_count=$(grep -Fxc "$elo_line" "$elo_file" || :)
  [ "$elo_count" -eq 1 ] ||
    fail "$elo_file: expected one exact line: $elo_line (found $elo_count)"
}

line_number_once() {
  lno_file=$1
  lno_pattern=$2
  lno_matches=$(grep -nFx "$lno_pattern" "$lno_file" || :)
  lno_count=$(printf '%s\n' "$lno_matches" | sed '/^$/d' | wc -l | tr -d ' ')
  [ "$lno_count" -eq 1 ] ||
    fail "$lno_file: expected one ordering anchor: $lno_pattern (found $lno_count)"
  printf '%s\n' "$lno_matches" | sed 's/:.*//'
}

extract_cleanup() {
  ec_file=$1
  ec_function=$2
  ec_output=$3
  awk -v header="$ec_function() {" '
    $0 == header { copying=1 }
    copying { print }
    copying && /^}$/ { exit }
  ' "$ec_file" >"$ec_output"
  [ -s "$ec_output" ] || fail "$ec_file: $ec_function is missing"
}

check_trap_inventory() {
  cti_file=$REPO_ROOT/$1
  cti_function=$2
  cti_cleanup=$3
  expect_line_once "$cti_file" "trap $cti_function EXIT"
  expect_line_once "$cti_file" "trap 'exit 130' INT"
  expect_line_once "$cti_file" "trap 'exit 143' TERM"
  expect_line_once "$cti_file" "trap 'exit 129' HUP"
  cti_count=$(grep -c '^trap ' "$cti_file" || :)
  [ "$cti_count" -eq 4 ] ||
    fail "$cti_file: expected exactly four top-level trap declarations (found $cti_count)"

  cti_body=$scratch/$cti_function
  extract_cleanup "$cti_file" "$cti_function" "$cti_body"
  for cti_line in \
    'cleanup_status=$?' \
    "trap '' HUP INT TERM" \
    'trap - EXIT' \
    "$cti_cleanup" \
    'exit "$cleanup_status"'; do
    expect_line_once "$cti_body" "  $cti_line"
  done

  cti_capture=$(line_number_once "$cti_body" '  cleanup_status=$?')
  cti_ignore=$(line_number_once "$cti_body" "  trap '' HUP INT TERM")
  cti_disable=$(line_number_once "$cti_body" '  trap - EXIT')
  cti_remove=$(line_number_once "$cti_body" "  $cti_cleanup")
  cti_exit=$(line_number_once "$cti_body" '  exit "$cleanup_status"')
  if [ "$cti_capture" -ne 2 ] || [ "$cti_capture" -ge "$cti_ignore" ] ||
     [ "$cti_ignore" -ge "$cti_disable" ] ||
     [ "$cti_disable" -ge "$cti_remove" ] ||
     [ "$cti_remove" -ge "$cti_exit" ]; then
    fail "$cti_file: cleanup must mask signals before disabling EXIT and preserve status"
  fi
}

check_trap_inventory ci/checks/orchestrators/golden-tests.sh \
  cleanup_golden_tests 'rm -rf "$TMP_ROOT"'
check_trap_inventory ci/checks/orchestrators/castle-tests.sh \
  cleanup_castle_tests 'rm -rf "$ORCHESTRATOR_TMP"'
check_trap_inventory ci/checks/orchestrators/poc-tests.sh \
  cleanup_poc_tests 'rm -rf "$ORCHESTRATOR_TMP"'
check_trap_inventory ci/checks/orchestrators/contrib-tests.sh \
  cleanup_contrib_tests 'rm -rf "$ORCHESTRATOR_TMP"'
check_trap_inventory ci/checks/orchestrators/emissions-tests.sh \
  cleanup_emissions_tests 'rm -rf "$ORCHESTRATOR_TMP"'
check_trap_inventory ci/checks/orchestrators/highlight-tokens.sh \
  cleanup_highlight_tokens 'rm -rf "$scratch"'
check_trap_inventory ci/checks/orchestrators/generative-tests.sh \
  cleanup_generative_tests 'rm -rf "$ORCHESTRATOR_TMP"'
check_trap_inventory ci/checks/orchestrators/builtin-docs.sh \
  cleanup_builtin_docs 'rm -f "$TMP"'
check_trap_inventory ci/checks/orchestrators/devcontainer-lifecycle.sh \
  cleanup_devcontainer_lifecycle 'rm -rf "$scratch"'
check_trap_inventory ci/checks/orchestrators/fmt-canonical-tree.sh \
  cleanup_fmt_canonical_tree 'rm -rf "$scratch"'
check_trap_inventory ci/checks/orchestrators/fmt-comment-conservation.sh \
  cleanup_fmt_comment_conservation 'rm -rf "$batch"'
check_trap_inventory ci/checks/orchestrators/host-docs-snippets.sh \
  cleanup_host_docs_snippets 'rm -rf "$WORK"'
check_trap_inventory ci/checks/orchestrators/runner-cache-hermeticity.sh \
  cleanup_runner_cache_hermeticity 'rm -rf "$tmp"'
check_trap_inventory ci/checks/orchestrators/rust-output-determinism.sh \
  cleanup_rust_output_determinism 'rm -rf "$tmp"'
check_trap_inventory ci/checks/repo-lint/dyn-load-host-surface-selftest.sh \
  cleanup_dyn_load_host_surface_selftest 'rm -rf "$scratch"'
check_trap_inventory test-data/goldens/00_success/exec_dyn_load_binding_origins/run.sh \
  cleanup_binding_origins 'rm -rf "$work"'
check_trap_inventory test-data/goldens/30_dependency_error/dep_retype_duplicate_selector/run.sh \
  cleanup_retype_duplicate_selector 'rm -rf "$scratch"'
check_trap_inventory test-data/goldens/30_dependency_error/dep_retype_conflicting_overlap/run.sh \
  cleanup_retype_conflicting_overlap 'rm -rf "$scratch"'

run_tests=$REPO_ROOT/ci/run-tests.sh
expect_line_once "$run_tests" 'trap cleanup_run_tests EXIT'
expect_line_once "$run_tests" "trap 'exit 130' INT"
expect_line_once "$run_tests" "trap 'exit 143' TERM"
expect_line_once "$run_tests" "trap 'exit 129' HUP"
run_tests_trap_count=$(grep -c '^trap ' "$run_tests" || :)
[ "$run_tests_trap_count" -eq 4 ] ||
  fail "$run_tests: expected exactly four top-level trap declarations (found $run_tests_trap_count)"

run_tests_cleanup=$scratch/run-tests-cleanup
extract_cleanup "$run_tests" cleanup_run_tests "$run_tests_cleanup"

for cleanup_line in \
  'cleanup_status=$?' \
  "trap '' HUP INT TERM" \
  'trap - EXIT' \
  'exit "$cleanup_status"'; do
  expect_line_once "$run_tests_cleanup" "  $cleanup_line"
done
expect_line_once "$run_tests_cleanup" \
  '    kill "$progress_ticker_pid" 2>/dev/null || true'
expect_line_once "$run_tests_cleanup" \
  '    wait "$progress_ticker_pid" 2>/dev/null || true'
expect_line_once "$run_tests_cleanup" \
  '    if ! : >"$run_tests_cancel_file"; then'
expect_line_once "$run_tests_cleanup" \
  '    wait "$run_tests_supervisor_pid" 2>/dev/null || true'
expect_line_once "$run_tests_cleanup" \
  '     [ -f "$run_tests_drained_marker" ]; then'
expect_line_once "$run_tests_cleanup" '      rm -rf "$TMPDIR_RUN"'

capture_line=$(line_number_once "$run_tests_cleanup" '  cleanup_status=$?')
ignore_line=$(line_number_once "$run_tests_cleanup" "  trap '' HUP INT TERM")
disable_line=$(line_number_once "$run_tests_cleanup" '  trap - EXIT')
kill_line=$(line_number_once "$run_tests_cleanup" \
  '    kill "$progress_ticker_pid" 2>/dev/null || true')
wait_line=$(line_number_once "$run_tests_cleanup" \
  '    wait "$progress_ticker_pid" 2>/dev/null || true')
cancel_line=$(line_number_once "$run_tests_cleanup" \
  '    if ! : >"$run_tests_cancel_file"; then')
supervisor_wait_line=$(line_number_once "$run_tests_cleanup" \
  '    wait "$run_tests_supervisor_pid" 2>/dev/null || true')
drained_line=$(line_number_once "$run_tests_cleanup" \
  '     [ -f "$run_tests_drained_marker" ]; then')
remove_line=$(line_number_once "$run_tests_cleanup" '      rm -rf "$TMPDIR_RUN"')
exit_line=$(line_number_once "$run_tests_cleanup" '  exit "$cleanup_status"')
if [ "$capture_line" -ne 2 ] || [ "$capture_line" -ge "$ignore_line" ] ||
   [ "$ignore_line" -ge "$disable_line" ] ||
   [ "$disable_line" -ge "$kill_line" ] ||
   [ "$kill_line" -ge "$wait_line" ] || [ "$wait_line" -ge "$cancel_line" ] ||
   [ "$cancel_line" -ge "$supervisor_wait_line" ] ||
   [ "$supervisor_wait_line" -ge "$drained_line" ] ||
   [ "$drained_line" -ge "$remove_line" ] ||
   [ "$remove_line" -ge "$exit_line" ]; then
  fail "$run_tests: cleanup status/trap/ticker/supervisor/removal order is invalid"
fi

for launch_line in \
  '    trap '\''[ -n "$run_tests_pending_status" ] || run_tests_pending_status=130'\'' INT' \
  '    trap '\''[ -n "$run_tests_pending_status" ] || run_tests_pending_status=143'\'' TERM' \
  '    trap '\''[ -n "$run_tests_pending_status" ] || run_tests_pending_status=129'\'' HUP' \
  '    run_tests_supervisor_pid=$!' \
  '    trap '\''exit 130'\'' INT' \
  '    trap '\''exit 143'\'' TERM' \
  '    trap '\''exit 129'\'' HUP' \
  '    [ -z "$run_tests_pending_status" ] || exit "$run_tests_pending_status"'; do
  expect_line_once "$run_tests" "$launch_line"
done
defer_line=$(line_number_once "$run_tests" \
  '    trap '\''[ -n "$run_tests_pending_status" ] || run_tests_pending_status=130'\'' INT')
first_spawn_line=$(line_number_once "$run_tests" \
  '      ) <&"$run_tests_stdin_reserved" &')
last_spawn_line=$(line_number_once "$run_tests" \
  '          sh "$SCRIPT_PATH" --__supervised-main "$@" 0<&- &')
pid_line=$(line_number_once "$run_tests" '    run_tests_supervisor_pid=$!')
full_line=$(line_number_once "$run_tests" "    trap 'exit 130' INT")
close_stdin_reservation_line=$(line_number_once "$run_tests" \
  '    close_run_tests_stdin_reservation')
pending_line=$(line_number_once "$run_tests" \
  '    [ -z "$run_tests_pending_status" ] || exit "$run_tests_pending_status"')
if [ "$defer_line" -ge "$first_spawn_line" ] ||
   [ "$last_spawn_line" -ge "$pid_line" ] || [ "$pid_line" -ge "$full_line" ] ||
   [ "$full_line" -ge "$close_stdin_reservation_line" ] ||
   [ "$close_stdin_reservation_line" -ge "$pending_line" ]; then
  fail "$run_tests: signal deferral does not close the supervisor launch race"
fi

printf 'shell-signal-trap-selftest: pass\n'
