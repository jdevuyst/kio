#!/bin/sh

# Keep target eligibility, sampling, filtering, ordering, and diagnostics
# stable while ensuring worklist setup indexes package targets once instead
# of probing the filesystem for every (case, implementation) pair. The
# RUN_EARLY fixtures separately pin parallel priority dispatch after selection
# while serial and interactive modes retain canonical execution and every mode
# retains canonical reporting.

set -eu

TAB=$(printf '\t')
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
RUN_TESTS_SH=${RUN_TESTS_SH:-"$REPO_ROOT/ci/run-tests.sh"}
SELFTEST_PATH=$SCRIPT_DIR/$(basename -- "$0")

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/run tests predispatch.XXXXXX")
async_pid='' async_gate='' async_status=''
stop_async_harness() {
  [ -n "${async_pid:-}" ] || return 0
  [ -z "${async_gate:-}" ] || : >"$async_gate/release"
  [ "${KIO_TEST_CLEANUP_MUTANT:-}" != skip-wait ] ||
    { async_pid='' async_gate=''; return 0; }
  if wait "$async_pid" >/dev/null 2>&1; then async_status=0; else async_status=$?; fi
  async_pid='' async_gate=''
}
finish() {
  trap '' HUP INT TERM
  trap - EXIT
  finish_status=$1
  [ "${KIO_TEST_CLEANUP_MUTANT:-}" = skip-stop ] || stop_async_harness
  if [ "$finish_status" -eq 0 ] &&
     [ "${async_status:-0}" -ne 0 ] &&
     [ "${KIO_TEST_CLEANUP_MUTANT:-}" != swallow-status ]; then finish_status=$async_status; fi
  finish_result=${KIO_TEST_CLEANUP_RESULT_DIR:-}; rm -rf "$scratch"
  if [ -n "$finish_result" ]; then
    printf '%s\n' "$finish_status" >"$finish_result/final-status"
    printf 'cleanup-complete\n' >>"$finish_result/events"
  fi
  exit "$finish_status"
}
[ "${KIO_TEST_CLEANUP_MUTANT:-}" = remove-exit-trap ] ||
  trap 'finish "$?"' EXIT
trap 'finish 1' HUP INT TERM
mkdir -p "$scratch/bin" "$scratch/cache" "$scratch/cases/bucket" \
  "$scratch/bad/invalid/workdir" "$scratch/package-shape" "$scratch/tmp"

resolve_executable() {
  re_name=$1 re_path=$PATH:
  while [ -n "$re_path" ]; do
    re_dir=${re_path%%:*}
    re_path=${re_path#*:}
    [ -n "$re_dir" ] || re_dir=.
    re_dir=$(CDPATH='' cd -- "$re_dir" 2>/dev/null && pwd -P) || continue
    if [ -f "$re_dir/$re_name" ] && [ -x "$re_dir/$re_name" ]; then
      printf '%s/%s\n' "$re_dir" "$re_name"
      return 0
    fi
  done
  return 1; }
real_find=$(resolve_executable find)
real_grep=$(resolve_executable grep)
real_xargs=$(resolve_executable xargs)
real_od=$(resolve_executable od)
KIO_TEST_REAL_OD=$real_od
export KIO_TEST_REAL_OD
test_scheduler_bin=$(TMPDIR="$scratch/tmp" sh "$REPO_ROOT/ci/schedule.sh" --prepare)
KIO_TEST_REAL_GREP=$real_grep
KIO_TEST_REAL_XARGS=$real_xargs
export KIO_TEST_REAL_GREP
export KIO_TEST_REAL_XARGS
cat >"$scratch/bin/find" <<'EOF'
#!/bin/sh
printf 'find\n' >>"$KIO_TEST_FIND_LOG"
if [ "${KIO_TEST_REQUIRE_DISCOVERY_PRINT:-0}" = 1 ]; then
  discovery=0
  for arg in "$@"; do
    [ "$arg" != expected.exit ] || discovery=1
  done
  if [ "$discovery" = 1 ]; then
    if [ "$#" -ne 14 ] ||
       [ "$1" != "$KIO_TEST_DISCOVERY_ROOT" ] ||
       [ "$2" != '(' ] ||
       [ "$3" != -name ] || [ "$4" != expected.exit ] ||
       [ "$5" != -type ] || [ "$6" != f ] ||
       [ "$7" != -o ] || [ "$8" != -name ] ||
       [ "$9" != '*.pkg.kio' ] ||
       [ "${10}" != -o ] || [ "${11}" != -name ] ||
       [ "${12}" != .kio-generated ] || [ "${13}" != ')' ] ||
       [ "${14}" != -print ]; then
      printf 'case and package discovery must use the exact combined printing walk\n' >&2
      exit 97
    fi
    if [ "${KIO_TEST_DUPLICATE_MARKER_SLASH:-0}" = 1 ]; then
      "$KIO_TEST_REAL_FIND" "$@" |
        while IFS= read -r discovery_path; do
          case "$discovery_path" in
            */expected.exit)
              printf '%s//expected.exit\n' "${discovery_path%/expected.exit}"
              ;;
            *) printf '%s\n' "$discovery_path" ;;
          esac
        done
      exit
    fi
  fi
fi
exec "$KIO_TEST_REAL_FIND" "$@"
EOF

cat >"$scratch/bin/grep" <<'EOF'
#!/bin/sh
if [ "$#" -eq 4 ] && [ "$1" = -E ] && [ "$2" = -f ]; then
  [ -z "${KIO_TEST_GREP_LOG:-}" ] || printf 'batch\n' >>"$KIO_TEST_GREP_LOG"
  case "${KIO_TEST_GREP_MODE:-}" in
    stderr)
      printf 'injected batch diagnostic\n' >&2
      ;;
    partial-status-2)
      IFS= read -r first_case <"$4" || first_case=
      [ -z "$first_case" ] || printf '%s\n' "$first_case"
      exit 2
      ;;
    partial-status-1)
      IFS= read -r first_case <"$4" || first_case=
      [ -z "$first_case" ] || printf '%s\n' "$first_case"
      exit 1
      ;;
  esac
elif [ "$#" -eq 3 ] && [ "$1" = -Eq ] && [ "$2" = -- ]; then
  [ -z "${KIO_TEST_GREP_LOG:-}" ] ||
    printf 'legacy\t%s\n' "$3" >>"$KIO_TEST_GREP_LOG"
fi
exec "$KIO_TEST_REAL_GREP" "$@"
EOF

cat >"$scratch/bin/xargs" <<'EOF'
#!/bin/sh
[ -z "${KIO_TEST_ABORT_RESULT_DIR:-}" ] ||
  printf '%s\n' "$$" >"$KIO_TEST_ABORT_RESULT_DIR/dispatcher-pid"
exec "$KIO_TEST_REAL_XARGS" "$@"
EOF

cat >"$scratch/bin/od" <<'EOF'
#!/bin/sh
[ -z "${KIO_TEST_OD_LOG:-}" ] || printf 'od\n' >>"$KIO_TEST_OD_LOG"
exec "$KIO_TEST_REAL_OD" "$@"
EOF

cat >"$scratch/bin/scheduler-must-not-run" <<'EOF'
#!/bin/sh
: >"$KIO_TEST_SCHEDULER_UNEXPECTED"
exit 97
EOF

cat >"$scratch/bin/kio-a" <<'EOF'
#!/bin/sh
[ -z "${KIO_TEST_KIO_CALL_LOG:-}" ] || printf '%s\n' "$*" >>"$KIO_TEST_KIO_CALL_LOG"
exit 0
EOF
cp "$scratch/bin/kio-a" "$scratch/bin/kio-b"

cat >"$scratch/bin/runner" <<'EOF'
#!/bin/sh
case_dir=${PWD%/workdir}
if [ -n "${KIO_TEST_ABORT_RESULT_DIR:-}" ] &&
   [ "${case_dir##*/}" = b_rust ]; then
  abort_result=$KIO_TEST_ABORT_RESULT_DIR
  case ${KIO_DEBUG_TYPED_CACHE_ROOT:-} in
    */shared-typed-cache)
      abort_run_root=${KIO_DEBUG_TYPED_CACHE_ROOT%/shared-typed-cache}
      ;;
    *)
      printf 'could not identify abort fixture run root\n' >&2
      exit 92
      ;;
  esac
  printf '%s\n' "$abort_run_root" >"$abort_result/run-root"
  printf '%s\n' "$$" >"$abort_result/descendant-pid"
  abort_complete() {
    if [ -f "$abort_run_root/units.keys" ]; then
      printf 'state-present\n' >"$abort_result/descendant-state"
    else
      printf 'state-missing\n' >"$abort_result/descendant-state"
    fi
    printf 'descendant-complete\n' >>"$abort_result/events"
    : >"$abort_result/descendant-complete"
    exit 0
  }
  trap abort_complete HUP INT TERM
  : >"$abort_result/entered"
  abort_waited=0
  abort_release_after=${KIO_TEST_ABORT_AUTO_RELEASE_AFTER:-15}
  while [ ! -f "$abort_result/release" ]; do
    abort_waited=$((abort_waited + 1))
    [ "$abort_waited" -lt "$abort_release_after" ] || : >"$abort_result/release"
    sleep 1
  done
  abort_complete
fi
if [ -n "${KIO_TEST_EXPECT_CALLER_FD7:-}" ]; then
  printf '%s\n' "${case_dir##*/}" >&7 || {
    printf 'runner did not inherit caller descriptor 7\n' >&2
    exit 91
  }
fi
if [ -n "${KIO_TEST_EXPECT_CALLER_FD9:-}" ]; then
  printf '%s\n' "${case_dir##*/}" >&9 || {
    printf 'runner did not inherit caller descriptor 9\n' >&2
    exit 91
  }
fi
if [ -n "${KIO_TEST_MISSING_DONE_CASE:-}" ] &&
   [ "${case_dir##*/}" = "$KIO_TEST_MISSING_DONE_CASE" ]; then
  # The fixture deliberately corrupts harness completion state, but worker
  # transport variables must not leak into the runner. Derive the test-owned
  # run root from the documented invocation-scoped cache root instead.
  case ${KIO_DEBUG_TYPED_CACHE_ROOT:-} in
    */shared-typed-cache)
      missing_run_root=${KIO_DEBUG_TYPED_CACHE_ROOT%/shared-typed-cache}
      ;;
    *)
      printf 'could not identify missing-done fixture run root\n' >&2
      exit 92
      ;;
  esac
  tab=$(printf '\t')
  missing_done_key=$(awk -F "$tab" -v case_dir="$case_dir" \
    '$2 == case_dir { print $1; exit }' "$missing_run_root/units.tsv")
  if [ -z "$missing_done_key" ]; then
    printf 'could not identify missing-done fixture unit\n' >&2
    exit 92
  fi
  missing_done_path=$missing_run_root/done/$missing_done_key
  if [ ! -d "$missing_done_path" ]; then
    mkdir "$missing_done_path" || exit
    : >"$KIO_TEST_MISSING_DONE_ARMED"
  fi
fi
last_arg=
for arg in "$@"; do
  last_arg=$arg
done
[ -z "${KIO_TEST_RUNNER_ARG_LOG:-}" ] || {
  : >"$KIO_TEST_RUNNER_ARG_LOG"
  printf '%s\n' "$@" >>"$KIO_TEST_RUNNER_ARG_LOG"
}
if [ -n "${KIO_TEST_PARALLEL_GATE_DIR:-}" ]; then
  "$KIO_TEST_PARALLEL_GATE_HELPER" "${case_dir##*/}" "$KIO_TARGET" || exit $?
fi
if [ -n "${KIO_TEST_SERIAL_GATE_DIR:-}" ] &&
   [ "${case_dir##*/}" = "$KIO_TEST_SERIAL_GATE_CASE" ]; then
  printf '%s\n' "$$" >"$KIO_TEST_SERIAL_GATE_DIR/descendant-pid"
  : >"$KIO_TEST_SERIAL_GATE_DIR/entered"
  gate_waited=0
  while [ ! -f "$KIO_TEST_SERIAL_GATE_DIR/release" ]; do
    gate_waited=$((gate_waited + 1))
    [ "$gate_waited" -lt 8 ] || {
      : >"$KIO_TEST_SERIAL_GATE_DIR/self-release"; : >"$KIO_TEST_SERIAL_GATE_DIR/release"; break
    }
    sleep 1
  done
  sleep 1
fi
printf '%s\t%s\n' "${case_dir##*/}" "${last_arg##*/}" \
  >>"$KIO_TEST_PREDISPATCH_LOG"
if [ "${case_dir##*/}" = "${KIO_TEST_FAIL_CASE:-}" ]; then
  printf 'broken\n'
else
  printf 'ok\n'
fi
EOF

cat >"$scratch/bin/parallel-gate" <<'EOF'
#!/bin/sh
set -eu

gate=$KIO_TEST_PARALLEL_GATE_DIR
case_name=$1
target=$2
mkdir -p "$gate/tokens"
token=$gate/tokens/$case_name-$target

# Hold both worker slots until the first two distinct (case, target) units
# have reached their runner. No later xargs unit can enter before the
# snapshot is committed, so the file records the actual first wave rather
# than completion order.
if mkdir "$token" 2>/dev/null; then
  waited=0
  while :; do
    token_count=0
    for token_dir in "$gate/tokens"/*; do
      [ -d "$token_dir" ] || continue
      token_count=$((token_count + 1))
    done
    [ "$token_count" -ge 2 ] && break
    waited=$((waited + 1))
    if [ "$waited" -ge 10 ]; then
      printf 'parallel gate timed out waiting for the first worker wave\n' >&2
      exit 1
    fi
    sleep 1
  done

  if (set -C; : >"$gate/snapshot-lock") 2>/dev/null; then
    for token_dir in "$gate/tokens"/*; do
      [ -d "$token_dir" ] || continue
      printf '%s\n' "${token_dir##*/}"
    done | LC_ALL=C sort >"$gate/first-wave.tmp"
    mv "$gate/first-wave.tmp" "$gate/first-wave"
  fi
fi

waited=0
while [ ! -f "$gate/first-wave" ]; do
  waited=$((waited + 1))
  if [ "$waited" -ge 10 ]; then
    printf 'parallel gate timed out waiting for the first-wave snapshot\n' >&2
    exit 1
  fi
  sleep 1
done
EOF

make_case() {
  mc_name=$1
  mc_mode=$2
  mc_targets=$3
  mc_dir=$scratch/cases/bucket/$mc_name
  mkdir -p "$mc_dir/workdir"
  printf '0\n' >"$mc_dir/expected.exit"
  printf 'ok\n' >"$mc_dir/expected.stdout"
  : >"$mc_dir/expected.stderr.ignore"
  case "$mc_mode" in
    standard)
      : >"$mc_dir/run.args"
      ;;
    custom)
      cat >"$mc_dir/run.sh" <<'EOF'
#!/bin/sh
if [ -n "${KIO_TEST_PARALLEL_GATE_DIR:-}" ]; then
  "$KIO_TEST_PARALLEL_GATE_HELPER" "${PWD##*/}" "$KIO_TARGET" || exit $?
fi
printf '%s\t%s\n' "${PWD##*/}" "$KIO_TARGET" \
  >>"$KIO_TEST_PREDISPATCH_LOG"
printf 'ok\n'
EOF
      chmod +x "$mc_dir/run.sh"
      ;;
  esac
  if [ "$mc_targets" != none ]; then
    {
      printf 'package %s;\n\n' "$mc_name"
      if [ "$mc_targets" != no-build ]; then
        printf 'build {\n'
        for mc_target in $mc_targets; do
          printf '  target %s {\n    out "out/%s/";\n  };\n' \
            "$mc_target" "$mc_target"
        done
        printf '}\n'
      fi
    } >"$mc_dir/workdir/$mc_name.pkg.kio"
  fi
}

make_case a_js standard 'js'
make_case b_rust standard 'rust'
make_case c_both standard 'js rust'
make_case d_agnostic custom none
make_case e_no_build standard no-build
: >"$scratch/cases/bucket/e_no_build/NO_BUILD_BLOCK"
make_case f_custom custom 'js rust'
make_case g_manifest_symlink custom none
: >"$scratch/cases/bucket/a_js/IS_KIO_PRIME"
: >"$scratch/cases/bucket/a_js/DYN_LOAD_PRIME"
: >"$scratch/cases/bucket/b_rust/DYN_LOAD_PRIME"
: >"$scratch/cases/bucket/f_custom/IS_KIO_PRIME"
: >"$scratch/cases/bucket/f_custom/RUN_EARLY"
: >"$scratch/cases/bucket/g_manifest_symlink/RUN_EARLY"

cat >"$scratch/symlink-target.pkg.kio" <<'EOF'
package decoy;

build {
  target js {
    out "out/js/";
  }
}
EOF
ln -s "$scratch/symlink-target.pkg.kio" \
  "$scratch/cases/bucket/g_manifest_symlink/workdir/decoy.pkg.kio"

mkdir "$scratch/linked-workdir"
cp "$scratch/symlink-target.pkg.kio" \
  "$scratch/linked-workdir/decoy.pkg.kio"
rmdir "$scratch/cases/bucket/d_agnostic/workdir"
ln -s "$scratch/linked-workdir" \
  "$scratch/cases/bucket/d_agnostic/workdir"

make_package_shape_case() {
  mpsc_name=$1
  mpsc_dir=$scratch/package-shape/$mpsc_name/case
  mkdir -p "$mpsc_dir/workdir"
  printf '0\n' >"$mpsc_dir/expected.exit"
  printf 'ok\n' >"$mpsc_dir/expected.stdout"
  : >"$mpsc_dir/expected.stderr.ignore"
  : >"$mpsc_dir/run.args"
}

make_package_shape_case missing
: >"$scratch/package-shape/missing/case/NO_BUILD_BLOCK"
: >"$scratch/package-shape/missing/case/expected.stdout"

make_package_shape_case symlink
ln -s "$scratch/symlink-target.pkg.kio" \
  "$scratch/package-shape/symlink/case/workdir/symlink.pkg.kio"

make_package_shape_case nested
mkdir -p "$scratch/package-shape/nested/case/workdir/child"
cat >"$scratch/package-shape/nested/case/workdir/child/child.pkg.kio" <<'EOF'
package child;

build {
  target js {
    out "out/js/";
  }
}
EOF

make_package_shape_case multiple
cat >"$scratch/package-shape/multiple/case/workdir/first.pkg.kio" <<'EOF'
package first;

build {
  target js {
    out "out/js/";
  }
}
EOF
cat >"$scratch/package-shape/multiple/case/workdir/second.pkg.kio" <<'EOF'
package second;

build {
  target js {
    out "out/js/";
  }
}
EOF

make_package_shape_case workdir-symlink
rm -rf "$scratch/package-shape/workdir-symlink/case/workdir"
mkdir -p "$scratch/package-shape/workdir-symlink/linked-workdir"
cat >"$scratch/package-shape/workdir-symlink/linked-workdir/root.pkg.kio" <<'EOF'
package root;

build {
  target js {
    out "out/js/";
  }
}
EOF
ln -s "$scratch/package-shape/workdir-symlink/linked-workdir" \
  "$scratch/package-shape/workdir-symlink/case/workdir"

make_package_shape_case generated-control
cat >"$scratch/package-shape/generated-control/case/workdir/root.pkg.kio" <<'EOF'
package root;

build {
  target js {
    out "out/js/";
  }
}
EOF
# Discovery prunes generated descendants, not the caller-supplied root itself.
: >"$scratch/package-shape/generated-control/case/workdir/.kio-generated"
mkdir -p "$scratch/package-shape/generated-control/case/workdir/cache/tree"
: >"$scratch/package-shape/generated-control/case/workdir/cache/.kio-generated"
cp "$scratch/package-shape/nested/case/workdir/child/child.pkg.kio" \
  "$scratch/package-shape/generated-control/case/workdir/cache/tree/child.pkg.kio"
for ignored_dir in out target .hidden; do
  mkdir -p "$scratch/package-shape/generated-control/case/workdir/$ignored_dir/tree"
  cp "$scratch/package-shape/nested/case/workdir/child/child.pkg.kio" \
    "$scratch/package-shape/generated-control/case/workdir/$ignored_dir/tree/child.pkg.kio"
done

for generated_marker_scope in case corpus; do
  make_package_shape_case "generated-$generated_marker_scope-marker"
  generated_marker_fixture=$scratch/package-shape/generated-$generated_marker_scope-marker
  cp -R "$scratch/package-shape/generated-control/case/workdir/." \
    "$generated_marker_fixture/case/workdir/"
  rm "$generated_marker_fixture/case/workdir/.kio-generated"
  case "$generated_marker_scope" in
    case) : >"$generated_marker_fixture/case/.kio-generated" ;;
    corpus) : >"$generated_marker_fixture/.kio-generated" ;;
  esac
done

make_package_shape_case ignored-package-symlinks
cp "$scratch/package-shape/generated-control/case/workdir/root.pkg.kio" \
  "$scratch/package-shape/ignored-package-symlinks/case/workdir/root.pkg.kio"
mkdir -p "$scratch/package-shape/ignored-package-symlinks/directory-target"
ln -s "$scratch/package-shape/ignored-package-symlinks/missing-target" \
  "$scratch/package-shape/ignored-package-symlinks/case/workdir/broken.pkg.kio"
ln -s "$scratch/package-shape/ignored-package-symlinks/directory-target" \
  "$scratch/package-shape/ignored-package-symlinks/case/workdir/directory.pkg.kio"

make_package_shape_case broken-generated-marker
cp "$scratch/package-shape/generated-control/case/workdir/root.pkg.kio" \
  "$scratch/package-shape/broken-generated-marker/case/workdir/root.pkg.kio"
mkdir -p "$scratch/package-shape/broken-generated-marker/case/workdir/cache/tree"
ln -s "$scratch/package-shape/broken-generated-marker/missing-marker-target" \
  "$scratch/package-shape/broken-generated-marker/case/workdir/cache/.kio-generated"
cp "$scratch/package-shape/nested/case/workdir/child/child.pkg.kio" \
  "$scratch/package-shape/broken-generated-marker/case/workdir/cache/tree/child.pkg.kio"

make_package_shape_case test-only-multiple
rm "$scratch/package-shape/test-only-multiple/case/run.args"
: >"$scratch/package-shape/test-only-multiple/case/run.test-only"
cp "$scratch/package-shape/generated-control/case/workdir/root.pkg.kio" \
  "$scratch/package-shape/test-only-multiple/case/workdir/root.pkg.kio"
mkdir -p "$scratch/package-shape/test-only-multiple/case/workdir/child"
cp "$scratch/package-shape/nested/case/workdir/child/child.pkg.kio" \
  "$scratch/package-shape/test-only-multiple/case/workdir/child/child.pkg.kio"

make_package_shape_case custom-multiple
rm "$scratch/package-shape/custom-multiple/case/run.args"
cp "$scratch/package-shape/generated-control/case/workdir/root.pkg.kio" \
  "$scratch/package-shape/custom-multiple/case/workdir/root.pkg.kio"
mkdir -p "$scratch/package-shape/custom-multiple/case/workdir/child"
cp "$scratch/package-shape/nested/case/workdir/child/child.pkg.kio" \
  "$scratch/package-shape/custom-multiple/case/workdir/child/child.pkg.kio"
cat >"$scratch/package-shape/custom-multiple/case/run.sh" <<'EOF'
#!/bin/sh
"$KIO_RUNNER" out/js
EOF
chmod +x "$scratch/package-shape/custom-multiple/case/run.sh"

cat >"$scratch/bad/invalid/workdir/.invalid.pkg.kio" <<'EOF'
package invalid;
EOF
: >"$scratch/bad/invalid/run.args"
printf '0\n' >"$scratch/bad/invalid/expected.exit"
: >"$scratch/bad/invalid/expected.stdout"
: >"$scratch/bad/invalid/expected.stderr.ignore"

chmod +x "$scratch/bin/find" "$scratch/bin/grep" "$scratch/bin/xargs" \
  "$scratch/bin/od" \
  "$scratch/bin/scheduler-must-not-run" \
  "$scratch/bin/kio-a" "$scratch/bin/kio-b" \
  "$scratch/bin/runner" "$scratch/bin/parallel-gate"

if ! KIO_CI_SCHEDULER_BIN="$scratch/bin/scheduler-must-not-run" \
  KIO_TEST_SCHEDULER_UNEXPECTED="$scratch/help-scheduler-ran" \
  TMPDIR="$scratch/tmp" \
  /bin/sh "$RUN_TESTS_SH" --help >"$scratch/help.stdout" 2>"$scratch/help.stderr" ||
   [ -e "$scratch/help-scheduler-ran" ]; then
  printf 'run-tests-predispatch-selftest: --help bootstrapped the native supervisor\n' >&2
  exit 1
fi

if ! KIO_CI_SCHEDULER_BIN="$scratch/bin/scheduler-must-not-run" \
  KIO_TEST_SCHEDULER_UNEXPECTED="$scratch/help-after-option-scheduler-ran" \
  TMPDIR="$scratch/tmp" \
  /bin/sh "$RUN_TESTS_SH" --jobs=1 --help \
    >"$scratch/help-after-option.stdout" \
    2>"$scratch/help-after-option.stderr" ||
   [ -e "$scratch/help-after-option-scheduler-ran" ]; then
  printf 'run-tests-predispatch-selftest: accepted option before --help bootstrapped the native supervisor\n' >&2
  exit 1
fi

set +e
KIO_CI_SCHEDULER_BIN="$scratch/bin/scheduler-must-not-run" \
  KIO_TEST_SCHEDULER_UNEXPECTED="$scratch/help-value-scheduler-ran" \
  TMPDIR="$scratch/tmp" \
  /bin/sh "$RUN_TESTS_SH" --exclude --help \
    >"$scratch/help-value.stdout" 2>"$scratch/help-value.stderr"
help_value_status=$?
set -e
if [ "$help_value_status" -ne 97 ] ||
   [ ! -e "$scratch/help-value-scheduler-ran" ]; then
  printf 'run-tests-predispatch-selftest: --help option value bypassed the native supervisor\n' >&2
  exit 1
fi

set +e
KIO_CI_SCHEDULER_BIN="$scratch/bin/scheduler-must-not-run" \
  KIO_TEST_SCHEDULER_UNEXPECTED="$scratch/failed-supervisor-ran" \
  TMPDIR="$scratch/tmp" \
  /bin/sh "$RUN_TESTS_SH" --cases-dir="$scratch/cases" \
    >"$scratch/failed-supervisor.stdout" 2>"$scratch/failed-supervisor.stderr"
failed_supervisor_status=$?
set -e
failed_supervisor_root=$(sed -n \
  's/^error: process-tree extinction was not established; preserved run state at //p' \
  "$scratch/failed-supervisor.stderr")
if [ "$failed_supervisor_status" -ne 97 ] ||
   [ ! -f "$scratch/failed-supervisor-ran" ] ||
   [ -z "$failed_supervisor_root" ] || [ ! -d "$failed_supervisor_root" ]; then
  cat "$scratch/failed-supervisor.stderr" >&2
  printf 'run-tests-predispatch-selftest: missing drain evidence did not preserve state\n' >&2
  exit 1
fi
rm -rf "$failed_supervisor_root"

set +e
KIO_CI_SCHEDULER_BIN="$scratch/bin/scheduler-must-not-run" \
  KIO_TEST_SCHEDULER_UNEXPECTED="$scratch/exhausted-fd-scheduler-ran" \
  TMPDIR="$scratch/tmp" \
  /bin/sh -c '
    exec 3</dev/null 4</dev/null 5</dev/null 6</dev/null
    exec 7</dev/null 8</dev/null 9</dev/null
    exec /bin/sh "$@"
  ' sh "$RUN_TESTS_SH" --cases-dir="$scratch/cases" \
    >"$scratch/exhausted-fd.stdout" 2>"$scratch/exhausted-fd.stderr"
exhausted_fd_status=$?
set -e
if [ "$exhausted_fd_status" -ne 1 ] ||
   [ -e "$scratch/exhausted-fd-scheduler-ran" ] ||
   ! grep -Fqx \
     'error: no portable file descriptor is available to preserve standard input' \
     "$scratch/exhausted-fd.stderr"; then
  cat "$scratch/exhausted-fd.stderr" >&2
  printf 'run-tests-predispatch-selftest: exhausted descriptor set did not fail before supervision\n' >&2
  exit 1
fi

run_harness() {
  rh_log=$1
  rh_find_log=$2
  rh_runner_log=$3
  shift 3
  : >"$rh_find_log"
  : >"$rh_runner_log"
  PATH="$scratch/bin:$PATH" \
    KIO_CI_SCHEDULE=DISABLE \
    KIO_CI_SCHEDULER_BIN="$test_scheduler_bin" \
    KIO_DEBUG_PROGRESS_INTERVAL=0 \
    KIO_TEST_FIND_LOG="$rh_find_log" \
    KIO_TEST_REAL_FIND="$real_find" \
    KIO_TEST_REQUIRE_DISCOVERY_PRINT=1 \
    KIO_TEST_DISCOVERY_ROOT="${KIO_TEST_CASES_DIR:-$scratch/cases}" \
    KIO_TEST_DUPLICATE_MARKER_SLASH="${KIO_TEST_DUPLICATE_MARKER_SLASH:-}" \
    KIO_TEST_GREP_LOG="${KIO_TEST_GREP_LOG:-}" \
    KIO_TEST_GREP_MODE="${KIO_TEST_GREP_MODE:-}" \
    KIO_TEST_PREDISPATCH_LOG="$rh_runner_log" \
    KIO_TEST_PARALLEL_GATE_DIR="${KIO_TEST_PARALLEL_GATE_DIR:-}" \
    KIO_TEST_PARALLEL_GATE_HELPER="$scratch/bin/parallel-gate" \
    KIO_TEST_SERIAL_GATE_DIR="${KIO_TEST_SERIAL_GATE_DIR:-}" \
    KIO_TEST_SERIAL_GATE_CASE="${KIO_TEST_SERIAL_GATE_CASE:-}" \
    KIO_TEST_MISSING_DONE_CASE="${KIO_TEST_MISSING_DONE_CASE:-}" \
    KIO_TEST_MISSING_DONE_ARMED="${KIO_TEST_MISSING_DONE_ARMED:-}" \
    KIO_TEST_EXPECT_CALLER_FD7="${KIO_TEST_EXPECT_CALLER_FD7:-}" \
    KIO_TEST_EXPECT_CALLER_FD9="${KIO_TEST_EXPECT_CALLER_FD9:-}" \
    KIO_TEST_CONSUME_STDIN_LOG="${KIO_TEST_CONSUME_STDIN_LOG:-}" \
    KIO_TEST_FAIL_CASE="${KIO_TEST_FAIL_CASE:-}" \
    KIO_TEST_NAMED_CHECK_LOG="${KIO_TEST_NAMED_CHECK_LOG:-}" \
    KIO_TEST_ABORT_RESULT_DIR="${KIO_TEST_ABORT_RESULT_DIR:-}" \
    KIO_TEST_ABORT_AUTO_RELEASE_AFTER="${KIO_TEST_ABORT_AUTO_RELEASE_AFTER:-}" \
    KIO_TEST_HARNESS_PID_FILE="${KIO_TEST_HARNESS_PID_FILE:-}" \
    RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
    CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
    TMPDIR="$scratch/tmp" \
    /bin/sh -c '
      [ -z "${KIO_TEST_HARNESS_PID_FILE:-}" ] ||
        printf "%s\n" "$$" >"$KIO_TEST_HARNESS_PID_FILE"
      exec /bin/sh "$@"
    ' sh "$RUN_TESTS_SH" \
      --cases-dir="${KIO_TEST_CASES_DIR:-$scratch/cases}" \
      --cache-base="$scratch/cache" \
      --jobs=1 \
      --impl-def="name=js-one,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=js" \
      --impl-def="name=js-two,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=js" \
      --impl-def="name=rust,kio=$scratch/bin/kio-b,runner=$scratch/bin/runner,target=rust" \
      --impl-def="name=rust-skip,kio=$scratch/bin/kio-b,runner=SKIP,target=rust" \
      "$@" >"$rh_log" 2>&1
}

run_package_shape_harness() {
  rpsh_label=$1
  rpsh_runner=$2
  shift 2
  rpsh_log=$scratch/package-shape/$rpsh_label/harness.log
  rpsh_find_log=$scratch/package-shape/$rpsh_label/find.log
  rpsh_kio_log=$scratch/package-shape/$rpsh_label/kio.log
  rpsh_runner_arg_log=$scratch/package-shape/$rpsh_label/runner-args.log
  : >"$rpsh_find_log"
  : >"$rpsh_kio_log"
  : >"$rpsh_runner_arg_log"
  PATH="$scratch/bin:$PATH" \
    KIO_CI_SCHEDULE=DISABLE \
    KIO_CI_SCHEDULER_BIN="$test_scheduler_bin" \
    KIO_DEBUG_PROGRESS_INTERVAL=0 \
    KIO_TEST_FIND_LOG="$rpsh_find_log" \
    KIO_TEST_REAL_FIND="$real_find" \
    KIO_TEST_KIO_CALL_LOG="$rpsh_kio_log" \
    KIO_TEST_RUNNER_ARG_LOG="$rpsh_runner_arg_log" \
    KIO_TEST_PREDISPATCH_LOG="$scratch/package-shape/$rpsh_label/runner.log" \
    RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
    CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
    TMPDIR="$scratch/tmp" \
    /bin/sh "$RUN_TESTS_SH" \
      --cases-dir="$scratch/package-shape/$rpsh_label" \
      --cache-base="$scratch/cache/package-shape-$rpsh_label" \
      --jobs=1 \
      --impl-def="name=only,kio=$scratch/bin/kio-a,runner=$rpsh_runner,target=js" \
      "$@" '^case$' \
      >"$rpsh_log" 2>&1
}

run_selector_harness() {
  rsh_label=$1
  rsh_mode=$2
  shift 2
  KIO_TEST_GREP_LOG=$scratch/$rsh_label.grep
  KIO_TEST_GREP_MODE=$rsh_mode
  : >"$KIO_TEST_GREP_LOG"
  rsh_status=0
  run_harness "$scratch/$rsh_label.log" "$scratch/$rsh_label.find" \
    "$scratch/$rsh_label.runner" "$@" || rsh_status=$?
  KIO_TEST_GREP_LOG=
  KIO_TEST_GREP_MODE=
  return "$rsh_status"
}

assert_fast_selector_log() {
  afsl_log=$1
  shift
  afsl_batches=$(grep -Fxc batch "$afsl_log" || true)
  [ "$afsl_batches" -eq 1 ] || return 1
  for afsl_filter in "$@"; do
    if grep -Fqx "legacy${TAB}$afsl_filter" "$afsl_log"; then
      return 1
    fi
  done
}

prepare_mutant_ci() {
  pmc_name=$1
  mutant_ci=$scratch/mutant-ci-$pmc_name
  rm -rf "$mutant_ci"
  mkdir -p "$mutant_ci"
  for pmc_entry in "$REPO_ROOT"/ci/*; do
    ln -s "$pmc_entry" "$mutant_ci/${pmc_entry##*/}"
  done
  rm "$mutant_ci/run-tests.sh"
  mutant_run_tests=$mutant_ci/run-tests.sh
}

run_abort_supervisor_mutant() {
  rasm_mode=$1
  prepare_mutant_ci "abort-$rasm_mode"
  cp "$RUN_TESTS_SH" "$mutant_run_tests"
  rm "$mutant_ci/schedule.sh"
  cat >"$mutant_ci/schedule.sh" <<'EOF'
#!/bin/sh
set -eu
if [ "${1:-}" != --supervise ]; then
  exec "$KIO_TEST_REAL_SCHEDULE" "$@"
fi
if [ "${1:-}" != --supervise ] || [ "${2:-}" != --drained-marker ] ||
   [ -z "${3:-}" ] || [ "${4:-}" != --cancel-file ] ||
   [ -z "${5:-}" ] || [ "${6:-}" != -- ]; then
  exit 2
fi
mutant_drained=$3
shift 6
[ "$KIO_TEST_ABORT_SUPERVISOR_MUTANT" != early-marker ] ||
  : >"$mutant_drained"
exec "$@"
EOF
  chmod +x "$mutant_ci/schedule.sh"
  rasm_original=$RUN_TESTS_SH
  RUN_TESTS_SH=$mutant_run_tests
  KIO_TEST_ABORT_SUPERVISOR_MUTANT=$rasm_mode
  KIO_TEST_ABORT_AUTO_RELEASE_AFTER=2
  KIO_TEST_REAL_SCHEDULE=$REPO_ROOT/ci/schedule.sh
  export KIO_TEST_ABORT_SUPERVISOR_MUTANT KIO_TEST_ABORT_AUTO_RELEASE_AFTER \
    KIO_TEST_REAL_SCHEDULE
  rasm_status=0
  run_abort_reap_case TERM 143 || rasm_status=$?
  RUN_TESTS_SH=$rasm_original
  unset KIO_TEST_ABORT_SUPERVISOR_MUTANT KIO_TEST_ABORT_AUTO_RELEASE_AFTER \
    KIO_TEST_REAL_SCHEDULE
  return "$rasm_status"
}

run_cleanup_case() {
  rcc_mode=$1 rcc_mutant=${2:-}
  rcc_result=$scratch/cleanup-${rcc_mutant:-good}-$rcc_mode
  rm -rf "$rcc_result"; mkdir -p "$rcc_result/gate"
  set +e
  RUN_TESTS_SH="$RUN_TESTS_SH" \
    KIO_TEST_CLEANUP_CHILD_MODE="$rcc_mode" \
    KIO_TEST_CLEANUP_MUTANT="$rcc_mutant" \
    KIO_TEST_CLEANUP_RESULT_DIR="$rcc_result" \
    TMPDIR="$scratch/tmp" \
    sh "$SELFTEST_PATH" >"$rcc_result/child.log" 2>&1
  rcc_status=$?
  set -e
  rcc_waited=0
  while [ ! -f "$rcc_result/harness-quiescent" ]; do
    rcc_waited=$((rcc_waited + 1)); [ "$rcc_waited" -lt 20 ] || break; sleep 1
  done
  if [ "$rcc_mode" = natural ]; then rcc_expected_status=7
  else rcc_expected_status=1; fi
  rcc_child_scratch=$(cat "$rcc_result/scratch-path" 2>/dev/null || :)
  rcc_failed=0
  if [ "$rcc_status" -ne "$rcc_expected_status" ] ||
     [ ! -f "$rcc_result/harness-quiescent" ] ||
     [ ! -f "$rcc_result/gate/release" ] ||
     [ -f "$rcc_result/gate/self-release" ] ||
     [ -z "$rcc_child_scratch" ] || [ -d "$rcc_child_scratch" ] ||
     ! grep -Fqx 0 "$rcc_result/harness-status" ||
     ! grep -Fqx "$rcc_expected_status" "$rcc_result/final-status" ||
     ! grep -Fqx 'harness-done cleanup-complete' "$rcc_result/events"; then rcc_failed=1; fi
  [ -z "$rcc_child_scratch" ] || rm -rf "$rcc_child_scratch"
  [ "$rcc_failed" -eq 0 ]
}

wait_for_abort_path() {
  wfap_path=$1
  wfap_waited=0
  while [ ! -e "$wfap_path" ]; do
    wfap_waited=$((wfap_waited + 1))
    [ "$wfap_waited" -lt 15 ] || return 1
    sleep 1
  done
}

run_abort_reap_case() {
  rarc_signal=$1
  rarc_expected_status=$2
  rarc_result=$scratch/abort-reap-$rarc_signal
  rm -rf "$rarc_result"
  mkdir -p "$rarc_result"
  : >"$rarc_result/events"

  (
    wait_for_abort_path "$rarc_result/harness-pid" || exit 1
    wait_for_abort_path "$rarc_result/dispatcher-pid" || exit 1
    wait_for_abort_path "$rarc_result/entered" || exit 1
    kill -s "$rarc_signal" "$(cat "$rarc_result/harness-pid")"
    # The repaired wrapper can reap the dispatcher between the controlling
    # signal and this causal current-base nudge. A vanished dispatcher is the
    # desired post-fix outcome.
    kill -s TERM "$(cat "$rarc_result/dispatcher-pid")" 2>/dev/null || :
    : >"$rarc_result/signal-sent"
  ) &
  rarc_helper_pid=$!

  set +e
  KIO_TEST_ABORT_RESULT_DIR=$rarc_result \
  KIO_TEST_HARNESS_PID_FILE=$rarc_result/harness-pid \
    run_harness "$rarc_result/harness.log" "$rarc_result/harness.find" \
      "$rarc_result/harness.runner" --jobs=2 -- \
      '^bucket/(b_rust|c_both)$'
  rarc_status=$?
  set -e
  if wait "$rarc_helper_pid"; then rarc_helper_status=0
  else rarc_helper_status=$?; fi

  printf 'harness-return\n' >>"$rarc_result/events"
  rarc_run_root=$(cat "$rarc_result/run-root" 2>/dev/null || :)
  rarc_descendant_pid=$(cat "$rarc_result/descendant-pid" 2>/dev/null || :)
  rarc_valid=1
  if [ "$rarc_status" -ne "$rarc_expected_status" ] ||
     [ "$rarc_helper_status" -ne 0 ] ||
     [ ! -f "$rarc_result/signal-sent" ] ||
     [ ! -f "$rarc_result/descendant-complete" ] ||
     ! grep -Fqx state-present "$rarc_result/descendant-state" 2>/dev/null ||
     [ -z "$rarc_run_root" ] || [ -e "$rarc_run_root" ] ||
     [ -z "$rarc_descendant_pid" ] ||
     kill -0 "$rarc_descendant_pid" 2>/dev/null ||
     ! printf 'descendant-complete\nharness-return\n' |
       diff -u - "$rarc_result/events" >/dev/null; then
    rarc_valid=0
  fi

  # A failing implementation can leave the held fixture behind. Release and
  # observe it before reporting the causal failure so the self-test itself
  # never leaks a process.
  : >"$rarc_result/release"
  wait_for_abort_path "$rarc_result/descendant-complete" || rarc_valid=0
  rarc_waited=0
  while [ -n "$rarc_descendant_pid" ] &&
        kill -0 "$rarc_descendant_pid" 2>/dev/null; do
    rarc_waited=$((rarc_waited + 1))
    [ "$rarc_waited" -lt 15 ] || { rarc_valid=0; break; }
    sleep 1
  done
  [ -z "$rarc_run_root" ] || rm -rf "$rarc_run_root"
  KIO_TEST_ABORT_RESULT_DIR=
  KIO_TEST_HARNESS_PID_FILE=
  [ "$rarc_valid" -eq 1 ]
}

if [ -n "${KIO_TEST_ABORT_ONLY_SIGNAL:-}" ]; then
  case "$KIO_TEST_ABORT_ONLY_SIGNAL" in
    HUP) abort_only_status=129 ;;
    INT) abort_only_status=130 ;;
    TERM) abort_only_status=143 ;;
    *) exit 2 ;;
  esac
  run_abort_reap_case "$KIO_TEST_ABORT_ONLY_SIGNAL" "$abort_only_status" &&
    exit 0
  cat "$rarc_result/harness.log" >&2
  exit 1
fi
if [ -n "${KIO_TEST_ABORT_MUTANT_ONLY:-}" ]; then
  if run_abort_supervisor_mutant "$KIO_TEST_ABORT_MUTANT_ONLY"; then
    exit 1
  fi
  exit 0
fi

if [ -n "${KIO_TEST_CLEANUP_CHILD_MODE:-}" ]; then
  cleanup_result=$KIO_TEST_CLEANUP_RESULT_DIR cleanup_gate=$KIO_TEST_CLEANUP_RESULT_DIR/gate
  printf '%s\n' "$scratch" >"$cleanup_result/scratch-path"
  : >"$cleanup_result/events"
  KIO_TEST_SERIAL_GATE_DIR=$cleanup_gate KIO_TEST_SERIAL_GATE_CASE=b_rust async_gate=$cleanup_gate
  (
    # This marker is the fixture process's final filesystem action. The
    # controller may therefore remove the child scratch as soon as it appears,
    # even when a cleanup mutant let this process outlive its parent shell.
    # Ignore signals here so no inherited handler can mark quiescence before
    # the finite harness has drained.
    trap '' HUP INT TERM
    trap \
      ': >"$cleanup_result/harness-quiescent.tmp"; mv "$cleanup_result/harness-quiescent.tmp" "$cleanup_result/harness-quiescent"' \
      EXIT
    cleanup_harness_status=0
    run_harness "$cleanup_result/harness.log" "$cleanup_result/harness.find" \
      "$cleanup_result/harness.runner" || cleanup_harness_status=$?
    printf '%s\n' "$cleanup_harness_status" >"$cleanup_result/harness-status"
    printf 'harness-done ' >>"$cleanup_result/events"
    exit 7
  ) &
  async_pid=$!
  cleanup_waited=0
  while [ ! -f "$cleanup_gate/entered" ]; do
    cleanup_waited=$((cleanup_waited + 1)); [ "$cleanup_waited" -lt 10 ] || exit 1; sleep 1
  done
  case "$KIO_TEST_CLEANUP_CHILD_MODE" in
    natural) exit 0 ;;
    HUP|TERM) kill -s "$KIO_TEST_CLEANUP_CHILD_MODE" "$$" ;;
  esac
  exit 99
fi
if [ -n "${KIO_TEST_CLEANUP_ONLY_MODE:-}" ]; then
  run_cleanup_case "$KIO_TEST_CLEANUP_ONLY_MODE" \
    "${KIO_TEST_CLEANUP_ONLY_MUTANT:-}" && exit 0
  exit 1
fi

make_package_shape_case unknown-target
unknown_target_dir=$scratch/package-shape/unknown-target
rm "$unknown_target_dir/case/run.args"
printf '40\n' >"$unknown_target_dir/case/expected.exit"
cat >"$unknown_target_dir/case/workdir/unknown.pkg.kio" <<'EOF'
package unknown;
build {
  target wasm { out "out/wasm/" }
}
EOF
cat >"$unknown_target_dir/case/run.sh" <<'EOF'
#!/bin/sh
printf 'case\t%s\n' "$KIO_TARGET" >>"$KIO_TEST_PREDISPATCH_LOG"
printf 'ok\n'
exit 40
EOF
chmod +x "$unknown_target_dir/case/run.sh"

# An unmarked, unmatched target still has no applicable implementation.
: >"$unknown_target_dir/runner.log"
run_package_shape_harness unknown-target "$scratch/bin/runner"
if grep -Eq '^pass \[|^FAIL \[' "$unknown_target_dir/harness.log"; then
  printf 'run-tests-predispatch-selftest: unmarked target gained applicability\n' >&2
  exit 1
fi

: >"$unknown_target_dir/case/UNKNOWN_BUILD_TARGET"
if ! run_package_shape_harness unknown-target "$scratch/bin/runner" \
    --impls=FULL_IMPL_MATRIX \
    --impl-def="name=rust,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=rust" \
    --impl-def="name=skip,kio=$scratch/bin/kio-a,runner=SKIP,target=js"; then
  cat "$unknown_target_dir/harness.log" >&2
  exit 1
fi
if ! printf 'case\tjs\ncase\trust\n' | diff -u - "$unknown_target_dir/runner.log" ||
   ! grep -Fqx '  only: 1 passed, 0 failed' "$unknown_target_dir/harness.log" ||
   ! grep -Fqx '  rust: 1 passed, 0 failed' "$unknown_target_dir/harness.log" ||
   ! grep -Fqx '  skip: 0 passed, 0 failed' "$unknown_target_dir/harness.log"; then
  cat "$unknown_target_dir/harness.log" >&2
  printf 'run-tests-predispatch-selftest: unknown-target diagnostic was not dispatched\n' >&2
  exit 1
fi

: >"$unknown_target_dir/selections"
for unknown_target_seed in 0 1; do
  : >"$unknown_target_dir/runner.log"
  KIO_DEBUG_SAMPLE_IMPL_SEED=$unknown_target_seed \
    run_package_shape_harness unknown-target "$scratch/bin/runner" \
      --impls=SAMPLE_IMPL \
      --impl-def="name=rust,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=rust" \
      --impl-def="name=skip,kio=$scratch/bin/kio-a,runner=SKIP,target=js"
  [ "$(wc -l <"$unknown_target_dir/runner.log" | tr -d ' ')" -eq 1 ]
  cat "$unknown_target_dir/runner.log" >>"$unknown_target_dir/selections"
done
sort "$unknown_target_dir/selections" >"$unknown_target_dir/selections.sorted"
printf 'case\tjs\ncase\trust\n' | diff -u - "$unknown_target_dir/selections.sorted"

# Case narrowing keeps the diagnostic's checks, without promoting its script.
mkdir -p "$unknown_target_dir/control/workdir"
cp "$scratch/package-shape/generated-control/case/workdir/root.pkg.kio" \
  "$unknown_target_dir/control/workdir/root.pkg.kio"
: >"$unknown_target_dir/control/run.args"
printf '0\n' >"$unknown_target_dir/control/expected.exit"
printf 'ok\n' >"$unknown_target_dir/control/expected.stdout"
: >"$unknown_target_dir/control/expected.stderr.ignore"
printf 'control\n' >"$unknown_target_dir/selected.cases"
printf '#!/bin/sh\n# ROUTING: case-binary\nexit 0\n' >"$unknown_target_dir/check.sh"
chmod +x "$unknown_target_dir/check.sh"
: >"$unknown_target_dir/runner.log"
run_package_shape_harness unknown-target "$scratch/bin/runner" \
  "--impl-case-set-file=$unknown_target_dir/selected.cases" \
  "--check=$unknown_target_dir/check.sh" '^control$'
printf 'control\tjs\n' | diff -u - "$unknown_target_dir/runner.log"
grep -Fqx 'pass [invariants@kio-a] case' "$unknown_target_dir/harness.log"
grep -Fqx '  only: 1 passed, 0 failed' "$unknown_target_dir/harness.log"

for unknown_target_invalid in standard success no-target no-build-marker no-manifest; do
  unknown_target_error='UNKNOWN_BUILD_TARGET requires custom run.sh and expected.exit 40'
  # shellcheck disable=SC2016 # Backticks are literal diagnostic text.
  case "$unknown_target_invalid" in
    standard)
      mv "$unknown_target_dir/case/run.sh" "$unknown_target_dir/run.saved"
      : >"$unknown_target_dir/case/run.args"
      ;;
    success) printf '0\n' >"$unknown_target_dir/case/expected.exit" ;;
    no-target)
      printf 'package unknown;\n' >"$unknown_target_dir/case/workdir/unknown.pkg.kio"
      unknown_target_error='declares no `build` block'
      ;;
    no-build-marker)
      : >"$unknown_target_dir/case/NO_BUILD_BLOCK"
      unknown_target_error='declares no `build` block'
      ;;
    no-manifest)
      rm "$unknown_target_dir/case/workdir/unknown.pkg.kio"
      unknown_target_error='declares no `build` block'
      ;;
  esac
  if run_package_shape_harness unknown-target "$scratch/bin/runner"; then
    printf 'run-tests-predispatch-selftest: invalid unknown-target marker accepted (%s)\n' \
      "$unknown_target_invalid" >&2
    exit 1
  fi
  grep -Fq "$unknown_target_error" "$unknown_target_dir/harness.log"
  case "$unknown_target_invalid" in
    standard)
      rm "$unknown_target_dir/case/run.args"
      mv "$unknown_target_dir/run.saved" "$unknown_target_dir/case/run.sh"
      ;;
    success) printf '40\n' >"$unknown_target_dir/case/expected.exit" ;;
  esac
done
printf 'run-tests-predispatch-selftest: unknown-target routing, sampling, narrowing, and marker guards passed\n'

for target_prefix_kind in plain single repeated tight; do
  case "$target_prefix_kind" in
    plain) target_prefix='  ' ;;
    single) target_prefix='  ; ' ;;
    repeated) target_prefix='  ;; ' ;;
    tight) target_prefix='  ;;' ;;
  esac
  target_prefix_label=target-prefix-$target_prefix_kind
  make_package_shape_case "$target_prefix_label"
  target_prefix_dir=$scratch/package-shape/$target_prefix_label
  {
    printf 'package prefix;\n\nbuild {\n'
    printf '%starget js {\n    out "out/js/"\n  };\n' "$target_prefix"
    printf '  // target rust {\n  docs {\n    md "target rust {"\n  }\n}\n'
  } >"$target_prefix_dir/case/workdir/prefix.pkg.kio"
  if ! run_package_shape_harness "$target_prefix_label" "$scratch/bin/runner" \
    --impl-def="name=decoy,kio=$scratch/bin/kio-b,runner=$scratch/bin/runner,target=rust"; then
    cat "$target_prefix_dir/harness.log" >&2
    printf 'run-tests-predispatch-selftest: %s target prefix lost dispatch\n' \
      "$target_prefix_kind" >&2
    exit 1
  fi
  if ! printf 'case\tjs\n' | diff -u - "$target_prefix_dir/runner.log" ||
     ! grep -Fqx 'pass [only] case' "$target_prefix_dir/harness.log"; then
    printf 'run-tests-predispatch-selftest: %s target prefix changed target selection\n' \
      "$target_prefix_kind" >&2
    exit 1
  fi
done

package_shape_failures=0
for package_shape_label in \
  missing symlink nested multiple workdir-symlink broken-generated-marker
do
  package_shape_runner=$scratch/bin/runner
  package_shape_expected=$package_shape_label
  case "$package_shape_label" in
    missing) package_shape_runner=SKIP ;;
    broken-generated-marker) package_shape_expected=multiple ;;
  esac
  if run_package_shape_harness "$package_shape_label" "$package_shape_runner"; then
    printf 'run-tests-predispatch-selftest: standard %s package shape reached dispatch\n' \
      "$package_shape_label" >&2
    package_shape_failures=$((package_shape_failures + 1))
  elif ! grep -Fq \
    "standard run.args case must contain exactly one discoverable package as a regular, non-symlink top-level workdir/*.pkg.kio file (found $package_shape_expected shape)" \
    "$scratch/package-shape/$package_shape_label/harness.log"; then
    cat "$scratch/package-shape/$package_shape_label/harness.log" >&2
    printf 'run-tests-predispatch-selftest: standard %s package shape lacked the pre-dispatch diagnostic\n' \
      "$package_shape_label" >&2
    package_shape_failures=$((package_shape_failures + 1))
  elif grep -Eq '^(test|build)( |$)' \
    "$scratch/package-shape/$package_shape_label/kio.log"; then
    printf 'run-tests-predispatch-selftest: standard %s package shape invoked the compiler after preflight failure\n' \
      "$package_shape_label" >&2
    package_shape_failures=$((package_shape_failures + 1))
  fi
done
[ "$package_shape_failures" -eq 0 ] || exit 1

generated_marker_failures=0
for generated_marker_label in generated-control generated-case-marker generated-corpus-marker; do
  generated_marker_result=$scratch/package-shape/$generated_marker_label
  if ! run_package_shape_harness "$generated_marker_label" "$scratch/bin/runner"; then
    cat "$generated_marker_result/harness.log" >&2
    printf 'run-tests-predispatch-selftest: %s changed discovery-root or descendant-marker handling\n' \
      "$generated_marker_label" >&2
    generated_marker_failures=$((generated_marker_failures + 1))
    continue
  fi
  sed -n '1,2p' "$generated_marker_result/runner-args.log" \
    >"$generated_marker_result/package-args.actual"
  printf '%s\n' --package-name root >"$generated_marker_result/package-args.expected"
  sed -n '/^pass \[/p; /^FAIL \[/p; /^warn \[/p' "$generated_marker_result/harness.log" \
    >"$generated_marker_result/report.actual"
  printf 'pass [only] case\n' >"$generated_marker_result/report.expected"
  printf 'case\tjs\n' >"$generated_marker_result/runner.expected"
  if ! cmp "$generated_marker_result/package-args.expected" \
        "$generated_marker_result/package-args.actual" ||
     ! cmp "$generated_marker_result/report.expected" "$generated_marker_result/report.actual" ||
     ! cmp "$generated_marker_result/runner.expected" "$generated_marker_result/runner.log"; then
    printf 'run-tests-predispatch-selftest: %s did not run exactly the root package once\n' \
      "$generated_marker_label" >&2
    generated_marker_failures=$((generated_marker_failures + 1))
  fi
done
[ "$generated_marker_failures" -eq 0 ] || exit 1

if ! run_package_shape_harness ignored-package-symlinks "$scratch/bin/runner"; then
  cat "$scratch/package-shape/ignored-package-symlinks/harness.log" >&2
  printf 'run-tests-predispatch-selftest: non-file package symlinks changed compiler package discovery\n' >&2
  exit 1
fi
sed -n '1,2p' \
  "$scratch/package-shape/ignored-package-symlinks/runner-args.log" \
  >"$scratch/package-shape/ignored-package-symlinks/package-args.actual"
printf '%s\n' --package-name root \
  >"$scratch/package-shape/ignored-package-symlinks/package-args.expected"
if ! cmp "$scratch/package-shape/ignored-package-symlinks/package-args.expected" \
  "$scratch/package-shape/ignored-package-symlinks/package-args.actual"; then
  printf 'run-tests-predispatch-selftest: ignored package symlinks changed the exact root identity\n' >&2
  exit 1
fi

if ! run_package_shape_harness test-only-multiple "$scratch/bin/runner"; then
  cat "$scratch/package-shape/test-only-multiple/harness.log" >&2
  printf 'run-tests-predispatch-selftest: run.test-only multi-package case was rejected by runner-only preflight\n' >&2
  exit 1
fi
if [ -s "$scratch/package-shape/test-only-multiple/runner-args.log" ]; then
  printf 'run-tests-predispatch-selftest: run.test-only multi-package case invoked the runner\n' >&2
  exit 1
fi

assert_test_only_execution() {
  ato_root=$scratch/package-shape/test-only-multiple
  sed -n '/^pass \[/p; /^FAIL \[/p' "$ato_root/harness.log" >"$ato_root/rows"
  printf 'pass [%s] case\n' "$1" >"$ato_root/expected.rows"
  sed -n '/^test$/p; /^build /p' "$ato_root/kio.log" >"$ato_root/commands"
  printf 'test\nbuild js\n' >"$ato_root/expected.commands"
  if ! cmp "$ato_root/expected.rows" "$ato_root/rows" ||
     ! cmp "$ato_root/expected.commands" "$ato_root/commands" ||
     [ -s "$ato_root/runner-args.log" ]; then
    cat "$ato_root/harness.log" "$ato_root/kio.log" >&2
    printf 'run-tests-predispatch-selftest: test-only row, compiler commands, or runner isolation changed\n' >&2
    exit 1
  fi
}
assert_test_only_execution only
run_package_shape_harness test-only-multiple SKIP
assert_test_only_execution only

: >"$scratch/test-only-selections"
for test_only_seed in 0 1; do
  KIO_DEBUG_SAMPLE_IMPL_SEED=$test_only_seed \
    run_package_shape_harness test-only-multiple "$scratch/bin/runner" \
      --impls=SAMPLE_IMPL \
      --impl-def="name=skip,kio=$scratch/bin/kio-a,runner=SKIP,target=js" \
      --impl-def="name=wrong-target,kio=$scratch/bin/kio-a,runner=SKIP,target=rust"
  test_only_selected=$(awk -F "$TAB" '/^sample-impl-map:/ { print $5 }' \
    "$scratch/package-shape/test-only-multiple/harness.log")
  assert_test_only_execution "$test_only_selected"
  printf '%s\n' "$test_only_selected" >>"$scratch/test-only-selections"
done
printf 'only\nskip\n' >"$scratch/test-only-selections.expected"
sort "$scratch/test-only-selections" >"$scratch/test-only-selections.sorted"
cmp "$scratch/test-only-selections.expected" "$scratch/test-only-selections.sorted"
printf 'run-tests-predispatch-selftest: test-only explicit SKIP and both sampled paths passed\n'

if ! run_package_shape_harness custom-multiple "$scratch/bin/runner"; then
  cat "$scratch/package-shape/custom-multiple/harness.log" >&2
  printf 'run-tests-predispatch-selftest: custom multi-package case was rejected by standard preflight\n' >&2
  exit 1
fi
printf '%s\n' out/js \
  >"$scratch/package-shape/custom-multiple/runner-args.expected"
if ! cmp "$scratch/package-shape/custom-multiple/runner-args.expected" \
  "$scratch/package-shape/custom-multiple/runner-args.log"; then
  printf 'run-tests-predispatch-selftest: custom multi-package case received an inferred package identity\n' >&2
  exit 1
fi

KIO_TEST_GREP_LOG=$scratch/unfiltered.grep
: >"$KIO_TEST_GREP_LOG"
if ! run_harness "$scratch/exact.log" "$scratch/exact.find" \
  "$scratch/exact.runner"; then
  KIO_TEST_GREP_LOG=
  cat "$scratch/exact.log" >&2
  printf 'run-tests-predispatch-selftest: exact fixture failed\n' >&2
  exit 1
fi
KIO_TEST_GREP_LOG=
if grep -Fqx batch "$scratch/unfiltered.grep"; then
  printf 'run-tests-predispatch-selftest: unfiltered planning entered the selector fast path\n' >&2
  exit 1
fi

# One corpus-wide discovery find inventories case markers and package-shape
# inputs; the other find counts final completion markers. Target indexing joins
# the first inventory rather than launching per-case or per-pair probes.
find_count=$(wc -l <"$scratch/exact.find" | tr -d ' ')
if [ "$find_count" -ne 2 ]; then
  printf 'run-tests-predispatch-selftest: worklist setup used %s find processes (expected exactly 2)\n' \
    "$find_count" >&2
  exit 1
fi

for expected in \
  '  js-one: 6 passed, 0 failed' \
  '  js-two: 6 passed, 0 failed' \
  '  rust: 6 passed, 0 failed' \
  '  rust-skip: 3 passed, 0 failed'
do
  if ! grep -Fqx "$expected" "$scratch/exact.log"; then
    cat "$scratch/exact.log" >&2
    printf 'run-tests-predispatch-selftest: missing exact summary row: %s\n' \
      "$expected" >&2
    exit 1
  fi
done

cat >"$scratch/expected.runner" <<'EOF'
a_js	js
a_js	js
b_rust	rust
c_both	js
c_both	js
c_both	rust
d_agnostic	js
d_agnostic	js
d_agnostic	rust
e_no_build	js
e_no_build	js
e_no_build	rust
f_custom	js
f_custom	js
f_custom	rust
g_manifest_symlink	js
g_manifest_symlink	js
g_manifest_symlink	rust
EOF
if ! diff -u "$scratch/expected.runner" "$scratch/exact.runner"; then
  printf 'run-tests-predispatch-selftest: serial canonical dispatch order changed\n' >&2
  exit 1
fi

cat >"$scratch/expected.report" <<'EOF'
pass [js-one] bucket/a_js
pass [js-two] bucket/a_js
pass [rust] bucket/b_rust
pass [rust-skip] bucket/b_rust
pass [js-one] bucket/c_both
pass [js-two] bucket/c_both
pass [rust] bucket/c_both
pass [rust-skip] bucket/c_both
pass [js-one] bucket/d_agnostic
pass [js-two] bucket/d_agnostic
pass [rust] bucket/d_agnostic
pass [js-one] bucket/e_no_build
pass [js-two] bucket/e_no_build
pass [rust] bucket/e_no_build
pass [rust-skip] bucket/e_no_build
pass [js-one] bucket/f_custom
pass [js-two] bucket/f_custom
pass [rust] bucket/f_custom
pass [js-one] bucket/g_manifest_symlink
pass [js-two] bucket/g_manifest_symlink
pass [rust] bucket/g_manifest_symlink
EOF

assert_report_order() {
  aro_log=$1
  aro_label=$2
  sed -n '/^pass \[/p' "$aro_log" >"$scratch/actual.report"
  if ! diff -u "$scratch/expected.report" "$scratch/actual.report"; then
    cat "$aro_log" >&2
    printf 'run-tests-predispatch-selftest: %s changed canonical report order\n' \
      "$aro_label" >&2
    exit 1
  fi
}

assert_report_order "$scratch/exact.log" 'buffered serial dispatch'

cat >"$scratch/worker.stdin" <<'EOF'
caller stdin one
caller stdin two
caller stdin three
EOF
prepare_mutant_ci consume-worker-stdin
stdin_worker_script=$mutant_run_tests
awk '
  {
    print
    if (!inserted && $0 == "if [ \"${1:-}\" = \"--__worker\" ]; then") {
      print "  _w_selftest_consumed=0"
      print "  while IFS= read -r _w_selftest_stdin; do"
      print "    _w_selftest_consumed=$((_w_selftest_consumed + 1))"
      print "  done"
      print "  printf \"%s\\t%s\\n\" \"${2:-}\" \"$_w_selftest_consumed\" >>\"$KIO_TEST_CONSUME_STDIN_LOG\""
      inserted = 1
    }
  }
  END { if (!inserted) exit 1 }
' "$RUN_TESTS_SH" >"$stdin_worker_script"
chmod +x "$stdin_worker_script"
KIO_TEST_CONSUME_STDIN_LOG=$scratch/consume-stdin.rows
: >"$KIO_TEST_CONSUME_STDIN_LOG"
original_run_tests=$RUN_TESTS_SH
RUN_TESTS_SH=$stdin_worker_script
if ! run_harness "$scratch/consume-stdin.log" "$scratch/consume-stdin.find" \
  "$scratch/consume-stdin.runner" <"$scratch/worker.stdin"; then
  RUN_TESTS_SH=$original_run_tests
  cat "$scratch/consume-stdin.log" >&2
  printf 'run-tests-predispatch-selftest: stdin-consuming serial fixture failed\n' >&2
  exit 1
fi
RUN_TESTS_SH=$original_run_tests
KIO_TEST_CONSUME_STDIN_LOG=
consume_attempts=$(wc -l <"$scratch/consume-stdin.rows" | tr -d ' ')
consume_unique=$(awk -F "$TAB" '!seen[$1]++ { count++ } END { print count + 0 }' \
  "$scratch/consume-stdin.rows")
consume_lines=$(awk -F "$TAB" '{ total += $2 } END { print total + 0 }' \
  "$scratch/consume-stdin.rows")
if [ "$consume_attempts" -ne 12 ] ||
   [ "$consume_unique" -ne "$consume_attempts" ] ||
   [ "$consume_lines" -ne 3 ] ||
   ! diff -u "$scratch/expected.runner" "$scratch/consume-stdin.runner"; then
  cat "$scratch/consume-stdin.log" >&2
  printf 'stdin attempts=%s unique=%s consumed-lines=%s\n' \
    "$consume_attempts" "$consume_unique" "$consume_lines" >&2
  cat "$scratch/consume-stdin.rows" >&2
  diff -u "$scratch/expected.runner" "$scratch/consume-stdin.runner" >&2 || :
  printf 'run-tests-predispatch-selftest: workers did not consume only caller stdin while dispatching every unit once\n' >&2
  exit 1
fi
assert_report_order "$scratch/consume-stdin.log" \
  'stdin-consuming buffered serial dispatch'

caller_fd7_log=$scratch/caller-fd7
caller_fd9_log=$scratch/caller-fd9
: >"$caller_fd7_log"; : >"$caller_fd9_log"
KIO_TEST_EXPECT_CALLER_FD7=1 KIO_TEST_EXPECT_CALLER_FD9=1
set +e
run_harness "$scratch/caller-fd9.log" "$scratch/caller-fd9.find" \
  "$scratch/caller-fd9.runner" 7>>"$caller_fd7_log" 9>>"$caller_fd9_log"
caller_fd9_status=$?
set -e
KIO_TEST_EXPECT_CALLER_FD7=''
KIO_TEST_EXPECT_CALLER_FD9=''
caller_fd7_rows=$(wc -l <"$caller_fd7_log" | tr -d ' ')
caller_fd9_rows=$(wc -l <"$caller_fd9_log" | tr -d ' ')
if [ "$caller_fd9_status" -ne 0 ] || [ "$caller_fd7_rows" -ne 9 ] ||
   [ "$caller_fd9_rows" -ne 9 ]; then
  cat "$scratch/caller-fd9.log" >&2
  printf 'run-tests-predispatch-selftest: buffered serial dispatch did not preserve caller descriptors 7 and 9\n' >&2
  exit 1
fi

serial_gate=$scratch/serial-gate
mkdir -p "$serial_gate"
KIO_TEST_SERIAL_GATE_DIR=$serial_gate
KIO_TEST_SERIAL_GATE_CASE=b_rust
async_gate=$serial_gate
run_harness "$scratch/serial-prefix.log" "$scratch/serial-prefix.find" \
  "$scratch/serial-prefix.runner" &
async_pid=$!
serial_waited=0
while [ ! -f "$serial_gate/entered" ]; do
  if ! kill -0 "$async_pid" 2>/dev/null; then
    wait "$async_pid" || true
    async_pid=
    cat "$scratch/serial-prefix.log" >&2
    printf 'run-tests-predispatch-selftest: serial prefix fixture exited before its gate\n' >&2
    exit 1
  fi
  serial_waited=$((serial_waited + 1))
  if [ "$serial_waited" -ge 10 ]; then
    printf 'run-tests-predispatch-selftest: serial prefix fixture did not reach its gate\n' >&2
    exit 1
  fi
  sleep 1
done
serial_descendant_pid=$(cat "$serial_gate/descendant-pid")
if [ "$serial_descendant_pid" = "$async_pid" ] ||
   ! kill -0 "$serial_descendant_pid" 2>/dev/null; then
  printf 'run-tests-predispatch-selftest: serial gate did not capture a live harness descendant\n' >&2
  exit 1
fi
sed -n '/^pass \[/p' "$scratch/serial-prefix.log" \
  >"$scratch/actual.serial-prefix.report"
sed -n '1,2p' "$scratch/expected.report" \
  >"$scratch/expected.serial-prefix.report"
if ! kill -0 "$async_pid" 2>/dev/null ||
   ! diff -u "$scratch/expected.serial-prefix.report" \
     "$scratch/actual.serial-prefix.report"; then
  cat "$scratch/serial-prefix.log" >&2
  printf 'run-tests-predispatch-selftest: serial output did not expose the completed canonical prefix\n' >&2
  exit 1
fi
stop_async_harness
serial_status=$async_status
KIO_TEST_SERIAL_GATE_DIR=
KIO_TEST_SERIAL_GATE_CASE=
if [ "$serial_status" -ne 0 ]; then
  cat "$scratch/serial-prefix.log" >&2
  printf 'run-tests-predispatch-selftest: serial prefix fixture failed\n' >&2
  exit 1
fi
if kill -0 "$serial_descendant_pid" 2>/dev/null; then
  printf 'run-tests-predispatch-selftest: serial gate descendant survived normal harness reap\n' >&2
  exit 1
fi
assert_report_order "$scratch/serial-prefix.log" 'progressive serial dispatch'

KIO_TEST_MISSING_DONE_CASE=b_rust
KIO_TEST_MISSING_DONE_ARMED=$scratch/missing-done.armed
set +e
run_harness "$scratch/missing-done.log" "$scratch/missing-done.find" \
  "$scratch/missing-done.runner"
missing_done_status=$?
set -e
KIO_TEST_MISSING_DONE_CASE=
KIO_TEST_MISSING_DONE_ARMED=
missing_done_diagnostics=$(grep -Ec \
  '^  !! 1 of [0-9][0-9]* units produced no completion marker ' \
  "$scratch/missing-done.log" || true)
if [ "$missing_done_status" -ne 1 ] ||
   [ ! -f "$scratch/missing-done.armed" ] ||
   [ "$missing_done_diagnostics" -ne 1 ]; then
  cat "$scratch/missing-done.log" >&2
  printf 'run-tests-predispatch-selftest: missing completion marker was not diagnosed exactly once\n' >&2
  exit 1
fi
assert_report_order "$scratch/missing-done.log" \
  'serial dispatch after a missing completion marker'

for streaming_mode in show-output update-expected; do
  case "$streaming_mode" in
    show-output) streaming_flag=--show-output ;;
    update-expected) streaming_flag=--update-expected ;;
  esac
  if ! run_harness "$scratch/$streaming_mode.log" \
    "$scratch/$streaming_mode.find" "$scratch/$streaming_mode.runner" \
    "$streaming_flag"; then
    cat "$scratch/$streaming_mode.log" >&2
    printf 'run-tests-predispatch-selftest: %s fixture failed\n' \
      "$streaming_mode" >&2
    exit 1
  fi
  assert_report_order "$scratch/$streaming_mode.log" "$streaming_mode"
done

parallel_gate=$scratch/parallel-gate
mkdir -p "$parallel_gate" "$scratch/parallel-bin"
KIO_TEST_REAL_MKDIR=$(resolve_executable mkdir)
KIO_TEST_REAL_MV=$(resolve_executable mv)
export KIO_TEST_REAL_MKDIR KIO_TEST_REAL_MV
cat >"$scratch/parallel-bin/mkdir" <<'EOF'
#!/bin/sh
# Successful directory creation can report an already-created racing peer.
# Publication must still elect exactly one writer.
if [ "$#" -eq 1 ] &&
   [ "$1" = "$KIO_TEST_PARALLEL_GATE_DIR/snapshot-lock" ]; then
  exec "$KIO_TEST_REAL_MKDIR" -p "$1"
fi
exec "$KIO_TEST_REAL_MKDIR" "$@"
EOF
cat >"$scratch/parallel-bin/mv" <<'EOF'
#!/bin/sh
if [ "$#" -eq 2 ] &&
   [ "$2" = "$KIO_TEST_PARALLEL_GATE_DIR/first-wave" ]; then
  : >"$KIO_TEST_PARALLEL_GATE_DIR/publisher-$$"
fi
exec "$KIO_TEST_REAL_MV" "$@"
EOF
chmod +x "$scratch/parallel-bin/mkdir" "$scratch/parallel-bin/mv"
KIO_TEST_PARALLEL_GATE_DIR=$parallel_gate
if ! PATH="$scratch/parallel-bin:$PATH" \
  run_harness "$scratch/parallel.log" "$scratch/parallel.find" \
  "$scratch/parallel.runner" --jobs=2; then
  cat "$scratch/parallel.log" >&2
  printf 'run-tests-predispatch-selftest: parallel fixture failed\n' >&2
  exit 1
fi
KIO_TEST_PARALLEL_GATE_DIR=
publisher_count=0
for publisher in "$parallel_gate"/publisher-*; do
  [ -f "$publisher" ] || continue
  publisher_count=$((publisher_count + 1))
done
if [ "$publisher_count" -ne 1 ]; then
  printf 'run-tests-predispatch-selftest: first wave had %s publishers (expected 1)\n' \
    "$publisher_count" >&2
  exit 1
fi
cat >"$scratch/expected.first-wave" <<'EOF'
f_custom-js
f_custom-rust
EOF
if ! diff -u "$scratch/expected.first-wave" "$parallel_gate/first-wave"; then
  cat "$scratch/parallel.log" >&2
  printf 'run-tests-predispatch-selftest: marked units were not the parallel first wave\n' >&2
  exit 1
fi
assert_report_order "$scratch/parallel.log" 'buffered parallel dispatch'

if ! run_selector_harness filtered '' --exclude='g_manifest_symlink$' -- \
  '^bucket/(a_js|f_custom|g_manifest_symlink)$'; then
  cat "$scratch/filtered.log" >&2
  printf 'run-tests-predispatch-selftest: include/exclude fixture failed\n' >&2
  exit 1
fi
if ! grep -Fqx '  js-one: 2 passed, 0 failed' "$scratch/filtered.log" ||
   ! grep -Fqx '  js-two: 2 passed, 0 failed' "$scratch/filtered.log" ||
   ! grep -Fqx '  rust: 1 passed, 0 failed' "$scratch/filtered.log" ||
   ! grep -Fqx '  rust-skip: 0 passed, 0 failed' "$scratch/filtered.log"; then
  cat "$scratch/filtered.log" >&2
  printf 'run-tests-predispatch-selftest: filtering changed applicability\n' >&2
  exit 1
fi
cat >"$scratch/expected.filtered.runner" <<'EOF'
a_js	js
a_js	js
f_custom	js
f_custom	js
f_custom	rust
EOF
if ! diff -u "$scratch/expected.filtered.runner" "$scratch/filtered.runner"; then
  cat "$scratch/filtered.log" >&2
  printf 'run-tests-predispatch-selftest: filtering changed serial canonical dispatch\n' >&2
  exit 1
fi

legacy_filter='^bucket/(a_js|f_custom|g_manifest_symlink)$'
legacy_filter_calls=$(grep -Fxc "legacy${TAB}$legacy_filter" \
  "$scratch/filtered.grep" || true)
if grep -Fqx batch "$scratch/filtered.grep" ||
   [ "$legacy_filter_calls" -ne 7 ]; then
  printf 'run-tests-predispatch-selftest: general regex did not retain the legacy matcher bound\n' >&2
  exit 1
fi

exact_a='^bucket/a_js$'
exact_b='^bucket/b_rust$'
exact_f='^bucket/f_custom$'
exact_g='^bucket/g_manifest_symlink$'
if ! run_selector_harness selector-fast '' \
  --exclude='g_manifest_symlink$' -- \
  "$exact_a" "$exact_a" "$exact_f" "$exact_g"; then
  cat "$scratch/selector-fast.log" >&2
  printf 'run-tests-predispatch-selftest: exact selector fast path failed\n' >&2
  exit 1
fi
if ! cmp -s "$scratch/filtered.log" "$scratch/selector-fast.log" ||
   ! cmp -s "$scratch/filtered.runner" "$scratch/selector-fast.runner" ||
   ! assert_fast_selector_log "$scratch/selector-fast.grep" \
     "$exact_a" "$exact_f" "$exact_g"; then
  printf 'run-tests-predispatch-selftest: exact selector fast path changed duplicate/exclude parity or matcher count\n' >&2
  exit 1
fi

run_32_safe_selectors() {
  set -- "$exact_a"
  safe_index=1
  while [ "$safe_index" -le 31 ]; do
    set -- "$@" "^bucket/missing_$safe_index\$"
    safe_index=$((safe_index + 1))
  done
  run_selector_harness selector-32-safe '' -- "$@" || return
  assert_fast_selector_log "$scratch/selector-32-safe.grep" "$@" || return
  safe_matcher_lines=$(wc -l <"$scratch/selector-32-safe.grep" | tr -d ' ')
  [ "$safe_matcher_lines" -eq 1 ]
}
if ! run_32_safe_selectors; then
  cat "$scratch/selector-32-safe.log" >&2
  printf 'run-tests-predispatch-selftest: 32 exact selectors did not use one bounded matcher\n' >&2
  exit 1
fi

if ! (
    cd "$scratch"
    KIO_TEST_CASES_DIR=cases
    run_selector_harness selector-relative '' -- "$exact_a" "$exact_f"
    run_selector_harness selector-relative-legacy '' -- \
      '^bucket/(a_js|f_custom)$'
  )
then
  printf 'run-tests-predispatch-selftest: relative cases-dir fixture failed\n' >&2
  exit 1
fi
if ! cmp -s "$scratch/selector-relative.log" \
     "$scratch/selector-relative-legacy.log" ||
   ! cmp -s "$scratch/selector-relative.runner" \
     "$scratch/selector-relative-legacy.runner" ||
   ! assert_fast_selector_log "$scratch/selector-relative.grep" \
     "$exact_a" "$exact_f"; then
  printf 'run-tests-predispatch-selftest: relative cases-dir parity changed\n' >&2
  exit 1
fi

KIO_TEST_CASES_DIR=$scratch/cases/
if ! run_selector_harness selector-trailing '' -- "$exact_a" "$exact_f" ||
   ! run_selector_harness selector-trailing-legacy '' -- \
     '^bucket/(a_js|f_custom)$'; then
  KIO_TEST_CASES_DIR=
  printf 'run-tests-predispatch-selftest: trailing-slash cases-dir fixture failed\n' >&2
  exit 1
fi
KIO_TEST_CASES_DIR=
if ! cmp -s "$scratch/selector-trailing.log" \
     "$scratch/selector-trailing-legacy.log" ||
   ! cmp -s "$scratch/selector-trailing.runner" \
     "$scratch/selector-trailing-legacy.runner" ||
   ! assert_fast_selector_log "$scratch/selector-trailing.grep" \
     "$exact_a" "$exact_f"; then
  printf 'run-tests-predispatch-selftest: trailing-slash cases-dir parity changed\n' >&2
  exit 1
fi

KIO_TEST_DUPLICATE_MARKER_SLASH=1
if ! run_selector_harness selector-repeated-marker-slash '' -- "$exact_a"; then
  KIO_TEST_DUPLICATE_MARKER_SLASH=
  cat "$scratch/selector-repeated-marker-slash.log" >&2
  printf 'run-tests-predispatch-selftest: repeated marker slash fixture failed\n' >&2
  exit 1
fi
KIO_TEST_DUPLICATE_MARKER_SLASH=
cat >"$scratch/expected.repeated-marker-slash.runner" <<'EOF'
a_js	js
a_js	js
EOF
if ! diff -u "$scratch/expected.repeated-marker-slash.runner" \
     "$scratch/selector-repeated-marker-slash.runner" ||
   ! assert_fast_selector_log "$scratch/selector-repeated-marker-slash.grep" \
     "$exact_a"; then
  printf 'run-tests-predispatch-selftest: repeated marker slash normalization changed exact selection\n' >&2
  exit 1
fi

exact_missing='^bucket/does_not_exist$'
legacy_missing='^bucket/does_not_[e]xist$'
if ! run_selector_harness selector-no-match '' -- "$exact_missing" ||
   ! run_selector_harness selector-no-match-legacy '' -- "$legacy_missing"; then
  printf 'run-tests-predispatch-selftest: no-match selector fixture failed\n' >&2
  exit 1
fi
if ! cmp -s "$scratch/selector-no-match.log" \
     "$scratch/selector-no-match-legacy.log" ||
   ! cmp -s "$scratch/selector-no-match.runner" \
     "$scratch/selector-no-match-legacy.runner" ||
   ! assert_fast_selector_log "$scratch/selector-no-match.grep" \
     "$exact_missing"; then
  printf 'run-tests-predispatch-selftest: clean grep status 1 did not preserve no-match parity\n' >&2
  exit 1
fi

KIO_TEST_CASES_DIR=$scratch/cases/.
if ! run_selector_harness selector-prime '' --prime-only -- \
     "$exact_a" "$exact_b" "$exact_f" ||
   ! run_selector_harness selector-prime-legacy '' --prime-only -- \
     '^bucket/(a_js|b_rust|f_custom)$'; then
  KIO_TEST_CASES_DIR=
  printf 'run-tests-predispatch-selftest: Prime selector fixture failed\n' >&2
  exit 1
fi
KIO_TEST_CASES_DIR=
if ! cmp -s "$scratch/selector-prime.log" \
     "$scratch/selector-prime-legacy.log" ||
   ! cmp -s "$scratch/selector-prime.runner" \
     "$scratch/selector-prime-legacy.runner" ||
   ! assert_fast_selector_log "$scratch/selector-prime.grep" \
     "$exact_a" "$exact_b" "$exact_f"; then
  printf 'run-tests-predispatch-selftest: exact path-map or Prime-marker parity changed\n' >&2
  exit 1
fi

if ! run_selector_harness selector-dyn '' --dyn-load-prime-only -- \
     "$exact_a" "$exact_b" "$exact_f" ||
   ! run_selector_harness selector-dyn-legacy '' --dyn-load-prime-only -- \
     '^bucket/(a_js|b_rust|f_custom)$'; then
  printf 'run-tests-predispatch-selftest: dyn-load-Prime selector fixture failed\n' >&2
  exit 1
fi
if ! cmp -s "$scratch/selector-dyn.log" \
     "$scratch/selector-dyn-legacy.log" ||
   ! cmp -s "$scratch/selector-dyn.runner" \
     "$scratch/selector-dyn-legacy.runner" ||
   ! assert_fast_selector_log "$scratch/selector-dyn.grep" \
     "$exact_a" "$exact_b" "$exact_f"; then
  printf 'run-tests-predispatch-selftest: dyn-load-Prime marker parity changed\n' >&2
  exit 1
fi

if ! run_selector_harness selector-fallback-control '' -- \
     "$exact_a" "$exact_f" ||
   ! run_selector_harness selector-fallback-stderr stderr -- \
     "$exact_a" "$exact_f" ||
   ! run_selector_harness selector-fallback-status partial-status-2 -- \
     "$exact_a" "$exact_f"; then
  printf 'run-tests-predispatch-selftest: selector fallback fixture failed\n' >&2
  exit 1
fi
for fallback_kind in stderr status; do
  if ! cmp -s "$scratch/selector-fallback-control.log" \
       "$scratch/selector-fallback-$fallback_kind.log" ||
     ! cmp -s "$scratch/selector-fallback-control.runner" \
       "$scratch/selector-fallback-$fallback_kind.runner" ||
     ! grep -Fqx "legacy${TAB}$exact_a" \
       "$scratch/selector-fallback-$fallback_kind.grep"; then
    printf 'run-tests-predispatch-selftest: %s fallback leaked diagnostics, partial state, or duplicate rows\n' \
      "$fallback_kind" >&2
    exit 1
  fi
done
if ! assert_fast_selector_log "$scratch/selector-fallback-control.grep" \
  "$exact_a" "$exact_f"; then
  printf 'run-tests-predispatch-selftest: fallback control did not fire only the batched matcher\n' >&2
  exit 1
fi

make_case 'h.dot' custom none
if ! run_selector_harness selector-metachar '' -- '^bucket/h.dot$' ||
   ! run_selector_harness selector-metachar-control '' -- \
     '^bucket/h[.]dot$'; then
  printf 'run-tests-predispatch-selftest: metacharacter selector fixture failed\n' >&2
  exit 1
fi
if ! cmp -s "$scratch/selector-metachar.log" \
     "$scratch/selector-metachar-control.log" ||
   ! cmp -s "$scratch/selector-metachar.runner" \
     "$scratch/selector-metachar-control.runner" ||
   grep -Fqx batch "$scratch/selector-metachar.grep" ||
   grep -Fqx batch "$scratch/selector-metachar-control.grep"; then
  printf 'run-tests-predispatch-selftest: regex metacharacter entered the exact-name fast path\n' >&2
  exit 1
fi

write_selector_mutant() {
  wsm_name=$1
  prepare_mutant_ci "$wsm_name"
  awk -v mutation="$wsm_name" '
  {
    if (mutation == "remap-order" &&
        index($0, "print substr($0, length(name) + 2)")) {
      print "             path = substr($0, length(name) + 2)"
      print "             if (name == \"bucket/a_js\") { held_path = path; next }"
      print "             print path"
      print "             if (name == \"bucket/f_custom\" && held_path != \"\") print held_path"
      mutated++
      next
    }
    if (mutation == "accept-partial-status-1" &&
        index($0, "[ \"$exact_grep_status\" -eq 1 ] && [ ! -s \"$exact_matches_tmp\" ]")) {
      print "         { [ \"$exact_grep_status\" -eq 1 ]; }; } &&"
      mutated++
      next
    }
    if (mutation == "accept-stderr" &&
        $0 == "       [ ! -s \"$exact_grep_stderr\" ]") {
      print "       :"
      mutated++
      next
    }
    if (mutation == "accept-status-2" &&
        index($0, "if { [ \"$exact_grep_status\" -eq 0 ] ||")) {
      print
      print "         [ \"$exact_grep_status\" -eq 2 ] ||"
      mutated++
      next
    }
    if (mutation == "broaden-classifier" &&
        index($0, "name !~ \"^[A-Za-z0-9_/-]+$\"")) {
      print "      if (name == \"\" || name !~ \"^[A-Za-z0-9_./-]+$\") exit 1"
      mutated++
      next
    }
    if (mutation == "single-trailing-slash-trim" &&
        index($0, "case_dir=${discovery_path%expected.exit}")) {
      print
      in_marker_normalization = 1
      next
    }
    if (mutation == "single-trailing-slash-trim" &&
        in_marker_normalization &&
        index($0, "case_dir=${case_dir%/}")) {
      print
      print "    break"
      mutated++
      in_marker_normalization = 0
      next
    }
    print
  }
  END { if (mutated != 1) exit 1 }
' "$RUN_TESTS_SH" >"$mutant_run_tests" || return
  chmod +x "$mutant_run_tests"
}

run_selector_mutant() {
  rsm_name=$1
  rsm_mode=$2
  shift 2
  write_selector_mutant "$rsm_name" || return
  rsm_original=$RUN_TESTS_SH
  RUN_TESTS_SH=$mutant_run_tests
  selector_mutant_status=0
  run_selector_harness "mutant-$rsm_name" "$rsm_mode" "$@" ||
    selector_mutant_status=$?
  RUN_TESTS_SH=$rsm_original
}

run_selector_mutant remap-order '' -- "$exact_a" "$exact_f"
if [ "$selector_mutant_status" -eq 0 ] &&
   cmp -s "$scratch/selector-fallback-control.runner" \
     "$scratch/mutant-remap-order.runner"; then
  printf 'run-tests-predispatch-selftest: remap-order mutation escaped\n' >&2
  exit 1
fi

run_selector_mutant accept-partial-status-1 partial-status-1 -- \
  "$exact_a" "$exact_f"
if [ "$selector_mutant_status" -eq 0 ] &&
   cmp -s "$scratch/selector-fallback-control.runner" \
     "$scratch/mutant-accept-partial-status-1.runner"; then
  printf 'run-tests-predispatch-selftest: partial-publication mutation escaped\n' >&2
  exit 1
fi

run_selector_mutant accept-stderr stderr -- "$exact_a" "$exact_f"
if [ "$selector_mutant_status" -ne 0 ] ||
   grep -Fqx "legacy${TAB}$exact_a" \
     "$scratch/mutant-accept-stderr.grep"; then
  printf 'run-tests-predispatch-selftest: stderr-fallback mutation escaped\n' >&2
  exit 1
fi

run_selector_mutant accept-status-2 partial-status-2 -- "$exact_a" "$exact_f"
if [ "$selector_mutant_status" -eq 0 ] &&
   cmp -s "$scratch/selector-fallback-control.runner" \
     "$scratch/mutant-accept-status-2.runner"; then
  printf 'run-tests-predispatch-selftest: status-fallback mutation escaped\n' >&2
  exit 1
fi

run_selector_mutant broaden-classifier '' -- '^bucket/h.dot$'
rm -rf "$scratch/cases/bucket/h.dot"
if [ "$selector_mutant_status" -ne 0 ] ||
   ! grep -Fqx batch "$scratch/mutant-broaden-classifier.grep"; then
  printf 'run-tests-predispatch-selftest: broadened-classifier mutation escaped\n' >&2
  exit 1
fi

KIO_TEST_DUPLICATE_MARKER_SLASH=1
run_selector_mutant single-trailing-slash-trim '' -- "$exact_a"
KIO_TEST_DUPLICATE_MARKER_SLASH=
if [ "$selector_mutant_status" -eq 0 ] &&
   diff -q "$scratch/expected.repeated-marker-slash.runner" \
     "$scratch/mutant-single-trailing-slash-trim.runner" >/dev/null 2>&1; then
  printf 'run-tests-predispatch-selftest: single-slash normalization mutation escaped\n' >&2
  exit 1
fi

# The marked custom case has both JS and Rust candidates. Its random choice
# must remain exactly one applicable implementation. Serial execution remains
# canonical; priority affects only the parallel route exercised above.
: >"$scratch/sample-impl.find"
: >"$scratch/sample-impl.runner"
: >"$scratch/sample-impl.od"
if ! PATH="$scratch/bin:$PATH" \
  KIO_CI_SCHEDULE=DISABLE \
  KIO_DEBUG_PROGRESS_INTERVAL=0 \
  KIO_TEST_FIND_LOG="$scratch/sample-impl.find" \
  KIO_TEST_REAL_FIND="$real_find" \
  KIO_TEST_REAL_OD="$real_od" \
  KIO_TEST_OD_LOG="$scratch/sample-impl.od" \
  KIO_TEST_PREDISPATCH_LOG="$scratch/sample-impl.runner" \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  TMPDIR="$scratch/tmp" \
  /bin/sh "$RUN_TESTS_SH" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --jobs=1 \
    --impls=SAMPLE_IMPL \
    --impl-def="name=js,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=js" \
    --impl-def="name=rust,kio=$scratch/bin/kio-b,runner=$scratch/bin/runner,target=rust" \
    -- '^bucket/(a_js|b_rust|f_custom)$' >"$scratch/sample-impl.log" 2>&1; then
  cat "$scratch/sample-impl.log" >&2
  printf 'run-tests-predispatch-selftest: SAMPLE_IMPL fixture failed\n' >&2
  exit 1
fi
sed -n '/^pass \[/p' "$scratch/sample-impl.log" >"$scratch/sample-impl.report"
sample_impl_rows=$(wc -l <"$scratch/sample-impl.report" | tr -d ' ')
for sample_case in a_js b_rust f_custom; do
  sample_case_rows=$(grep -Ec " bucket/$sample_case\$" \
    "$scratch/sample-impl.report" || true)
  if [ "$sample_case_rows" -ne 1 ]; then
    cat "$scratch/sample-impl.log" >&2
    printf 'run-tests-predispatch-selftest: SAMPLE_IMPL selected %s rows for %s (expected 1)\n' \
      "$sample_case_rows" "$sample_case" >&2
    exit 1
  fi
done
first_sample_runner_case=$(sed -n '1s/	.*//p' "$scratch/sample-impl.runner")
if [ "$sample_impl_rows" -ne 3 ] ||
   [ "$first_sample_runner_case" != a_js ] ||
   [ "$(wc -l <"$scratch/sample-impl.od" | tr -d ' ')" -ne 1 ] ||
   grep -Fq 'sample-impl-map:' "$scratch/sample-impl.log"; then
  cat "$scratch/sample-impl.log" >&2
  printf 'run-tests-predispatch-selftest: SAMPLE_IMPL changed selection or serial canonical dispatch\n' >&2
  exit 1
fi

assert_seeded_impl_execution() {
  asie_label=$1
  asie_count=$2
  asie_log=$scratch/$asie_label.log
  asie_map=$scratch/$asie_label.map
  sed -n '/^sample-impl-map:/p' "$asie_log" >"$asie_map"
  if ! awk -F "$TAB" -v count="$asie_count" '
      NF != 5 || seen[$4]++ { invalid = 1 }
      END { exit invalid || NR != count }
    ' "$asie_map"; then
    cat "$asie_log" >&2
    printf 'run-tests-predispatch-selftest: %s mapping has missing, duplicate, or malformed rows\n' \
      "$asie_label" >&2
    return 1
  fi
  awk -F "$TAB" '{ printf "pass [%s] %s\n", $5, $4 }' \
    "$asie_map" >"$scratch/$asie_label.expected.report"
  sed -n '/^pass \[/p; /^FAIL \[/p; /^warn \[/p' "$asie_log" \
    >"$scratch/$asie_label.report"
  if ! diff -u "$scratch/$asie_label.expected.report" \
      "$scratch/$asie_label.report"; then
    printf 'run-tests-predispatch-selftest: %s completed result rows differ from its mapping\n' \
      "$asie_label" >&2
    return 1
  fi
  awk -F "$TAB" '
    {
      target = $5
      if (target == "js-a" || target == "js-b") target = "js"
      if (target != "js" && target != "rust") exit 1
      sub(/^bucket\//, "", $4)
      printf "%s\t%s\n", $4, target
    }
  ' "$asie_map" >"$scratch/$asie_label.expected.runner" || return 1
  if ! diff -u "$scratch/$asie_label.expected.runner" \
      "$scratch/$asie_label.runner"; then
    printf 'run-tests-predispatch-selftest: %s runner invocations differ from its mapping\n' \
      "$asie_label" >&2
    return 1
  fi
}

run_seeded_impl_fixture() {
  rsif_label=$1
  rsif_seed=$2
  : >"$scratch/$rsif_label.runner"
  PATH="$scratch/bin:$PATH" \
    KIO_CI_SCHEDULE=DISABLE \
    KIO_DEBUG_PROGRESS_INTERVAL=0 \
    KIO_DEBUG_SAMPLE_IMPL_SEED="$rsif_seed" \
    KIO_TEST_FIND_LOG="$scratch/$rsif_label.find" \
    KIO_TEST_REAL_FIND="$real_find" \
    KIO_TEST_REAL_OD="$real_od" \
    KIO_TEST_OD_LOG="$scratch/sample-impl.od" \
    KIO_TEST_PREDISPATCH_LOG="$scratch/$rsif_label.runner" \
    RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
    CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
    TMPDIR="$scratch/tmp" \
    /bin/sh "$RUN_TESTS_SH" \
      --cases-dir="$scratch/cases" \
      --cache-base="$scratch/cache" \
      --jobs=1 \
      --impls=SAMPLE_IMPL \
      --impl-def="name=js,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=js" \
      --impl-def="name=rust,kio=$scratch/bin/kio-b,runner=$scratch/bin/runner,target=rust" \
      -- '^bucket/(a_js|b_rust|f_custom)$' >"$scratch/$rsif_label.log" 2>&1
}

for seeded_spec in zero-a:0 zero-b:0 max:4294967295 move-a:1 move-b:1; do
  seeded_label=${seeded_spec%%:*}
  seeded_value=${seeded_spec#*:}
  if ! run_seeded_impl_fixture "seeded-impl-$seeded_label" "$seeded_value"; then
    cat "$scratch/seeded-impl-$seeded_label.log" >&2
    printf 'run-tests-predispatch-selftest: seeded SAMPLE_IMPL fixture failed for %s\n' \
      "$seeded_value" >&2
    exit 1
  fi
  assert_seeded_impl_execution "seeded-impl-$seeded_label" 3 || exit 1
done

if ! cmp -s "$scratch/seeded-impl-zero-a.map" \
      "$scratch/seeded-impl-zero-b.map" ||
   ! cmp -s "$scratch/seeded-impl-move-a.map" \
      "$scratch/seeded-impl-move-b.map" ||
   [ "$(wc -l <"$scratch/seeded-impl-zero-a.map" | tr -d ' ')" -ne 3 ] ||
   [ "$(grep -Fxc 'sample-impl-map:	direct/regular	cases	bucket/a_js	js' \
       "$scratch/seeded-impl-zero-a.map" || true)" -ne 1 ] ||
   [ "$(grep -Fxc 'sample-impl-map:	direct/regular	cases	bucket/b_rust	rust' \
       "$scratch/seeded-impl-zero-a.map" || true)" -ne 1 ] ||
   [ "$(grep -Fxc 'sample-impl-map:	direct/regular	cases	bucket/f_custom	rust' \
       "$scratch/seeded-impl-zero-a.map" || true)" -ne 1 ] ||
   [ "$(grep -Fxc 'sample-impl-map:	direct/regular	cases	bucket/f_custom	js' \
       "$scratch/seeded-impl-move-a.map" || true)" -ne 1 ] ||
   [ "$(wc -l <"$scratch/sample-impl.od" | tr -d ' ')" -ne 1 ]; then
  printf 'run-tests-predispatch-selftest: deterministic SAMPLE_IMPL mapping is incomplete, unstable, unmoved, or reached randomness\n' >&2
  exit 1
fi

run_seeded_impl_domain_fixture() {
  rsidf_label=$1
  rsidf_mode=$2
  rsidf_scope=$3
  rsidf_seed=$4
  : >"$scratch/$rsidf_label.runner"
  case "$rsidf_mode" in
    regular) set -- ;;
    direct-prime) set -- --prime-only ;;
    dyn-load-prime) set -- --dyn-load-prime-only ;;
    *) return 2 ;;
  esac
  PATH="$scratch/bin:$PATH" \
    KIO_CI_SCHEDULE=DISABLE \
    KIO_DEBUG_PROGRESS_INTERVAL=0 \
    KIO_DEBUG_SAMPLE_IMPL_SEED="$rsidf_seed" \
    KIO_DEBUG_SAMPLE_IMPL_SCOPE="$rsidf_scope" \
    KIO_TEST_FIND_LOG="$scratch/$rsidf_label.find" \
    KIO_TEST_REAL_FIND="$real_find" \
    KIO_TEST_REAL_OD="$real_od" \
    KIO_TEST_OD_LOG="$scratch/sample-impl.od" \
    KIO_TEST_PREDISPATCH_LOG="$scratch/$rsidf_label.runner" \
    RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
    CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
    TMPDIR="$scratch/tmp" \
    /bin/sh "$RUN_TESTS_SH" \
      --cases-dir="$scratch/cases" \
      --cache-base="$scratch/cache" \
      --jobs=1 \
      --impls=SAMPLE_IMPL \
      --impl-def="name=js-a,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=js" \
      --impl-def="name=js-b,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=js" \
      --impl-def="name=rust,kio=$scratch/bin/kio-b,runner=$scratch/bin/runner,target=rust" \
      "$@" -- '^bucket/f_custom$' >"$scratch/$rsidf_label.log" 2>&1
}

: >"$scratch/cases/bucket/f_custom/DYN_LOAD_PRIME"
if ! run_seeded_impl_domain_fixture seeded-domain-regular regular direct 0 ||
   ! run_seeded_impl_domain_fixture seeded-domain-prime direct-prime direct 0 ||
   ! run_seeded_impl_domain_fixture seeded-domain-dyn-prime \
     dyn-load-prime direct 0 ||
   ! run_seeded_impl_domain_fixture seeded-domain-corpus regular \
     checks/orchestrators/golden-tests 0; then
  printf 'run-tests-predispatch-selftest: seeded SAMPLE_IMPL domain fixture failed\n' >&2
  exit 1
fi

if ! grep -Fqx \
      'sample-impl-map:	direct/regular	cases	bucket/f_custom	js-a' \
      "$scratch/seeded-domain-regular.log" ||
   ! grep -Fqx \
      'sample-impl-map:	direct/direct-prime	cases	bucket/f_custom	js-b' \
      "$scratch/seeded-domain-prime.log" ||
   ! grep -Fqx \
      'sample-impl-map:	direct/dyn-load-prime	cases	bucket/f_custom	rust' \
      "$scratch/seeded-domain-dyn-prime.log" ||
   ! grep -Fqx \
      'sample-impl-map:	checks/orchestrators/golden-tests/regular	goldens	bucket/f_custom	js-a' \
      "$scratch/seeded-domain-corpus.log"; then
  cat "$scratch/seeded-domain-regular.log" >&2
  cat "$scratch/seeded-domain-prime.log" >&2
  cat "$scratch/seeded-domain-dyn-prime.log" >&2
  cat "$scratch/seeded-domain-corpus.log" >&2
  printf 'run-tests-predispatch-selftest: seeded SAMPLE_IMPL phase/corpus domain did not affect the selected mapping\n' >&2
  exit 1
fi
for seeded_domain in regular prime dyn-prime corpus; do
  assert_seeded_impl_execution "seeded-domain-$seeded_domain" 1 || exit 1
done
rm "$scratch/cases/bucket/f_custom/DYN_LOAD_PRIME"

for invalid_seed in '' 00 01 -1 4294967296 1x; do
  if run_seeded_impl_fixture invalid-seed "$invalid_seed"; then
    printf 'run-tests-predispatch-selftest: invalid SAMPLE_IMPL seed accepted: <%s>\n' \
      "$invalid_seed" >&2
    exit 1
  fi
  if [ -s "$scratch/invalid-seed.runner" ] ||
     ! grep -Fq 'KIO_DEBUG_SAMPLE_IMPL_SEED must be canonical decimal in 0..4294967295' \
       "$scratch/invalid-seed.log"; then
    cat "$scratch/invalid-seed.log" >&2
    printf 'run-tests-predispatch-selftest: invalid SAMPLE_IMPL seed reached dispatch or lacked its diagnostic\n' >&2
    exit 1
  fi
done

KIO_DEBUG_SAMPLE_IMPL_SEED=0
export KIO_DEBUG_SAMPLE_IMPL_SEED
set +e
run_harness "$scratch/seed-without-sample.log" \
  "$scratch/seed-without-sample.find" "$scratch/seed-without-sample.runner"
seed_without_sample_status=$?
set -e
unset KIO_DEBUG_SAMPLE_IMPL_SEED
if [ "$seed_without_sample_status" -ne 2 ] ||
   [ -s "$scratch/seed-without-sample.runner" ] ||
   ! grep -Fq 'KIO_DEBUG_SAMPLE_IMPL_SEED requires --impls=SAMPLE_IMPL' \
     "$scratch/seed-without-sample.log"; then
  cat "$scratch/seed-without-sample.log" >&2
  printf 'run-tests-predispatch-selftest: debug implementation seed without SAMPLE_IMPL reached dispatch or lacked its diagnostic\n' >&2
  exit 1
fi

for repeat in one two; do
  run_harness "$scratch/seeded-$repeat.log" "$scratch/seeded-$repeat.find" \
    "$scratch/seeded-$repeat.runner" --sample-cases=1 --case-seed=fixed
done
if ! cmp -s "$scratch/seeded-one.log" "$scratch/seeded-two.log" ||
   ! cmp -s "$scratch/seeded-one.runner" "$scratch/seeded-two.runner"; then
  printf 'run-tests-predispatch-selftest: seeded case sampling is not reproducible\n' >&2
  exit 1
fi
cat >"$scratch/expected.seeded.runner" <<'EOF'
a_js	js
a_js	js
EOF
if ! grep -Fqx '  bucket/a_js' "$scratch/seeded-one.log" ||
   ! diff -u "$scratch/expected.seeded.runner" "$scratch/seeded-one.runner"; then
  cat "$scratch/seeded-one.log" >&2
  printf 'run-tests-predispatch-selftest: RUN_EARLY promoted a sampled-out case into impl execution\n' >&2
  exit 1
fi

make_case h_known_failing standard js
: >"$scratch/cases/bucket/h_known_failing/DYN_LOAD_PRIME"
: >"$scratch/cases/bucket/h_known_failing/KNOWN_FAILING"

named_check=$scratch/named-case-binary.sh
cat >"$named_check" <<'EOF'
#!/bin/sh
# ROUTING: case-binary
set -eu
printf '%s\n' "${PWD##*/}" >>"$KIO_TEST_NAMED_CHECK_LOG"
EOF
chmod +x "$named_check"
named_manifest=$scratch/named.cases
printf 'bucket/a_js\n' >"$named_manifest"

named_selection_oracle() {
  nso_label=$1
  nso_log=$scratch/$nso_label.log
  nso_runner=$scratch/$nso_label.runner
  nso_checks=$scratch/$nso_label.checks
  KIO_TEST_FAIL_CASE=h_known_failing
  KIO_TEST_NAMED_CHECK_LOG=$nso_checks
  : >"$nso_checks"
  if ! run_harness "$nso_log" "$scratch/$nso_label.find" "$nso_runner" \
      "--check=$named_check" "--impl-case-set-file=$named_manifest" \
      --dyn-load-prime-only -- \
      '^bucket/(a_js|b_rust|h_known_failing)$'; then
    KIO_TEST_FAIL_CASE=
    KIO_TEST_NAMED_CHECK_LOG=
    return 1
  fi
  KIO_TEST_FAIL_CASE=
  KIO_TEST_NAMED_CHECK_LOG=
  cat >"$scratch/expected.named.runner" <<'EOF'
a_js	js
a_js	js
h_known_failing	js
h_known_failing	js
EOF
  cat >"$scratch/expected.named.checks" <<'EOF'
a_js
b_rust
EOF
  diff -u "$scratch/expected.named.runner" "$nso_runner" >/dev/null 2>&1 &&
    diff -u "$scratch/expected.named.checks" "$nso_checks" >/dev/null 2>&1 &&
    [ "$(grep -Fc 'KNOWN_FAILING: tracked bug still reproduces' "$nso_log" || true)" -eq 2 ]
}

if ! named_selection_oracle named-selection; then
  cat "$scratch/named-selection.log" >&2
  printf 'run-tests-predispatch-selftest: named impl-tier selection, case-binary retention, or KNOWN_FAILING pinning changed\n' >&2
  exit 1
fi

KIO_TEST_FAIL_CASE=
KIO_TEST_NAMED_CHECK_LOG=$scratch/named-stale.checks
named_stale_status=0
run_harness "$scratch/named-stale.log" "$scratch/named-stale.find" \
  "$scratch/named-stale.runner" "--check=$named_check" \
  "--impl-case-set-file=$named_manifest" --dyn-load-prime-only -- \
  '^bucket/(a_js|b_rust|h_known_failing)$' || named_stale_status=$?
KIO_TEST_NAMED_CHECK_LOG=
if [ "$named_stale_status" -ne 1 ] ||
   ! grep -Fq 'KNOWN_FAILING but now passes' "$scratch/named-stale.log"; then
  cat "$scratch/named-stale.log" >&2
  printf 'run-tests-predispatch-selftest: stale KNOWN_FAILING named-set fixture did not fail\n' >&2
  exit 1
fi

assert_manifest_rejected() {
  amr_label=$1
  amr_manifest=$2
  shift 2
  amr_status=0
  run_harness "$scratch/manifest-$amr_label.log" \
    "$scratch/manifest-$amr_label.find" \
    "$scratch/manifest-$amr_label.runner" \
    "--impl-case-set-file=$amr_manifest" "$@" || amr_status=$?
  if [ "$amr_status" -ne 2 ] ||
     [ -s "$scratch/manifest-$amr_label.runner" ]; then
    cat "$scratch/manifest-$amr_label.log" >&2
    printf 'run-tests-predispatch-selftest: invalid named manifest reached dispatch: %s\n' \
      "$amr_label" >&2
    exit 1
  fi
}

: >"$scratch/empty.cases"
assert_manifest_rejected empty "$scratch/empty.cases"
printf '%s\n' bucket/a_js bucket/a_js >"$scratch/duplicate.cases"
assert_manifest_rejected duplicate "$scratch/duplicate.cases"
printf '../bucket/a_js\n' >"$scratch/unsafe.cases"
assert_manifest_rejected unsafe "$scratch/unsafe.cases"
printf '%s\n' bucket/b_rust bucket/a_js >"$scratch/unsorted.cases"
assert_manifest_rejected unsorted "$scratch/unsorted.cases"
printf '%s\n' 10 2 >"$scratch/numeric-sorted.cases"
numeric_sorted_status=0
run_harness "$scratch/manifest-numeric-sorted.log" \
  "$scratch/manifest-numeric-sorted.find" \
  "$scratch/manifest-numeric-sorted.runner" \
  "--impl-case-set-file=$scratch/numeric-sorted.cases" ||
  numeric_sorted_status=$?
if [ "$numeric_sorted_status" -ne 2 ] ||
   grep -Fq 'must be non-empty, normalized, safe, sorted, and unique' \
     "$scratch/manifest-numeric-sorted.log" ||
   ! grep -Fq 'member outside the canonical pass cohort' \
     "$scratch/manifest-numeric-sorted.log"; then
  cat "$scratch/manifest-numeric-sorted.log" >&2
  printf 'run-tests-predispatch-selftest: C-sorted numeric manifest was rejected as unsorted\n' >&2
  exit 1
fi
printf '%s\n' 2 10 >"$scratch/numeric-unsorted.cases"
assert_manifest_rejected numeric-unsorted "$scratch/numeric-unsorted.cases"
ln -s "$named_manifest" "$scratch/symlink.cases"
assert_manifest_rejected symlink "$scratch/symlink.cases"
assert_manifest_rejected relative relative.cases
printf 'bucket/f_custom\n' >"$scratch/outside.cases"
assert_manifest_rejected outside "$scratch/outside.cases" \
  --dyn-load-prime-only
assert_manifest_rejected mutual-exclusion "$named_manifest" \
  --sample-cases=1

prepare_mutant_ci named-ignore
# shellcheck disable=SC2016 # mutation matches literal run-tests variables
sed 's/if grep -Fqx "$named_case" "$IMPL_CASE_SET_FILE" ||/if [ -n "$named_case" ] ||/' \
  "$RUN_TESTS_SH" >"$mutant_run_tests"
chmod +x "$mutant_run_tests"
if cmp -s "$RUN_TESTS_SH" "$mutant_run_tests"; then
  printf 'run-tests-predispatch-selftest: named-selector ignore mutant did not apply\n' >&2
  exit 1
fi
canonical_run_tests=$RUN_TESTS_SH
RUN_TESTS_SH=$mutant_run_tests
if named_selection_oracle named-ignore-mutant; then
  printf 'run-tests-predispatch-selftest: ignored named selector mutation escaped\n' >&2
  exit 1
fi
RUN_TESTS_SH=$canonical_run_tests

prepare_mutant_ci named-early-filter
awk '
  !applied && $0 == "filter_count=$requested_filter_count" {
    print
    print "  mutant_filtered=$TMPDIR_RUN/cases.filtered.early"
    print "  : >\"$mutant_filtered\""
    print "  while IFS= read -r mutant_case; do"
    print "    printf \047%s/%s\\n\047 \"$CASES_DIR\" \"$mutant_case\" >>\"$mutant_filtered\""
    print "  done <\"$IMPL_CASE_SET_FILE\""
    print "  filtered_cases_file=$mutant_filtered"
    applied=1
    next
  }
  { print }
  END { if (!applied) exit 1 }
' "$RUN_TESTS_SH" >"$mutant_run_tests"
chmod +x "$mutant_run_tests"
RUN_TESTS_SH=$mutant_run_tests
if named_selection_oracle named-early-filter-mutant; then
  printf 'run-tests-predispatch-selftest: early named-filter mutation escaped\n' >&2
  exit 1
fi
RUN_TESTS_SH=$canonical_run_tests

set +e
PATH="$scratch/bin:$PATH" \
  KIO_CI_SCHEDULE=DISABLE KIO_DEBUG_PROGRESS_INTERVAL=0 \
  KIO_TEST_FIND_LOG="$scratch/bad.find" KIO_TEST_REAL_FIND="$real_find" \
  KIO_TEST_PREDISPATCH_LOG="$scratch/bad.runner" \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  TMPDIR="$scratch/tmp" \
  /bin/sh "$RUN_TESTS_SH" \
    --cases-dir="$scratch/bad" \
    --cache-base="$scratch/cache" \
    --jobs=1 \
    --impl-def="name=js,kio=$scratch/bin/kio-a,runner=$scratch/bin/runner,target=js" \
    >"$scratch/bad.log" 2>&1
bad_status=$?
set -e
# shellcheck disable=SC2016 # backticks are literal diagnostic text
if [ "$bad_status" -ne 1 ] ||
   ! grep -Fq 'declares no `build` block' "$scratch/bad.log"; then
  cat "$scratch/bad.log" >&2
  printf 'run-tests-predispatch-selftest: missing-build diagnostic changed\n' >&2
  exit 1
fi

abort_int_probe=$scratch/abort-int-probe
abort_int_resumed=$scratch/abort-int-resumed
set +e
sh -c '
  trap '\''printf "trapped\n" >"$1"; exit 130'\'' INT
  kill -s INT "$$"
  printf "resumed\n" >"$2"
  exit 99
' sh "$abort_int_probe" "$abort_int_resumed" >/dev/null 2>&1
abort_int_probe_status=$?
set -e
if [ "$abort_int_probe_status" -eq 130 ] && [ -f "$abort_int_probe" ] &&
   [ ! -e "$abort_int_resumed" ]; then
  abort_cases='HUP:129 INT:130 TERM:143'
elif [ "$abort_int_probe_status" -eq 99 ] && [ -f "$abort_int_resumed" ]; then
  # ci/all.sh launches each check from an asynchronous shell. POSIX shells
  # inherit SIGINT ignored in that context and cannot install a handler for it.
  # HUP and TERM still exercise the shell wrapper here; the explicit focused
  # mode and native supervisor tests retain the INT-specific evidence.
  abort_cases='HUP:129 TERM:143'
else
  printf 'run-tests-predispatch-selftest: cannot classify inherited INT disposition (status %s)\n' \
    "$abort_int_probe_status" >&2
  exit 1
fi

for abort_case in $abort_cases; do
  abort_signal=${abort_case%%:*}
  abort_status=${abort_case#*:}
  run_abort_reap_case "$abort_signal" "$abort_status" || {
    cat "$rarc_result/harness.log" >&2
    printf 'run-tests-predispatch-selftest: %s abort returned before its worker subtree was quiescent\n' \
      "$abort_signal" >&2
    exit 1
  }
done
for abort_mutant in missing-supervision early-marker; do
  if run_abort_supervisor_mutant "$abort_mutant"; then
    printf 'run-tests-predispatch-selftest: abort supervisor mutation %s escaped\n' \
      "$abort_mutant" >&2
    exit 1
  fi
done

for cleanup_mode in natural HUP TERM; do
  run_cleanup_case "$cleanup_mode" || {
    cat "$rcc_result/child.log" >&2
    printf 'run-tests-predispatch-selftest: %s cleanup failed\n' "$cleanup_mode" >&2; exit 1
  }
done
for cleanup_mutation in natural:remove-exit-trap natural:skip-stop \
  natural:skip-wait natural:swallow-status
do
  cleanup_mode=${cleanup_mutation%%:*} cleanup_mutant=${cleanup_mutation#*:}
  run_cleanup_case "$cleanup_mode" "$cleanup_mutant" && {
    printf 'run-tests-predispatch-selftest: cleanup mutation %s escaped\n' "$cleanup_mutant" >&2; exit 1
  }
done
printf 'run-tests-predispatch-selftest: ok\n'
