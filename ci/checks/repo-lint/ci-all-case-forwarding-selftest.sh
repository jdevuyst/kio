#!/bin/sh
#
# Prove ci/all.sh keeps global case coverage and per-corpus overrides on
# separate axes when it forwards arguments to discovered corpus jobs. The
# fixture runs only fake jobs; no compiler or corpus command is invoked.
#
# POSIX sh only.

set -eu

# A parent ci/all.sh exports live scheduler state. This fixture owns an
# isolated fake Git-common directory and must not inherit the parent's state
# or compiler wrappers.
unset \
  KIO_CI_SCHEDULE \
  KIO_CI_SCHEDULE_DIR \
  KIO_CI_SCHEDULE_HELD \
  KIO_CI_SCHEDULE_JOBS \
  KIO_CI_SCHEDULE_COMPILER_JOBS \
  KIO_CI_SCHEDULER_BIN \
  KIO_CI_SERIALIZE_CARGO \
  RUSTC_WRAPPER \
  RUSTC_WORKSPACE_WRAPPER \
  CARGO_BUILD_RUSTC_WRAPPER \
  CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
  KIO_TEST_RUNNER_COMPILER_WRAPPER

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
ALL_SH=$REPO_ROOT/ci/all.sh
CPU_SUM=$REPO_ROOT/ci/infra/ci-time-cpu-sum.sh
WORKFLOW=$REPO_ROOT/.github/workflows/ci.yml

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/ci-all-case-forwarding-selftest.XXXXXX") || {
  printf 'ci-all-case-forwarding-selftest: cannot create scratch directory\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

directory_has_entry() {
  dhe_dir=$1
  for dhe_entry in "$dhe_dir"/* "$dhe_dir"/.[!.]* "$dhe_dir"/..?*; do
    if [ -f "$dhe_entry" ] || [ -d "$dhe_entry" ] || [ -L "$dhe_entry" ]; then
      return 0
    fi
  done
  return 1
}

fixture=$scratch/repo
mkdir -p \
  "$fixture/ci/infra" \
  "$fixture/ci/checks/orchestrators" \
  "$fixture/ci/checks/orchestrators/lib" \
  "$fixture/ci/checks/orchestrators/case-coverage/dyn-load-prime" \
  "$fixture/ci/checks/repo-lint" \
  "$fixture/ci/checks/hygiene" \
  "$fixture/git-common" \
  "$fixture/bin" \
  "$scratch/tmp"
cp "$ALL_SH" "$fixture/ci/all.sh"
cp "$CPU_SUM" "$fixture/ci/infra/ci-time-cpu-sum.sh"
cp "$REPO_ROOT/ci/checks/orchestrators/lib/coverage-policy.sh" \
  "$REPO_ROOT/ci/checks/orchestrators/lib/orchestrator-registry.tsv" \
  "$REPO_ROOT/ci/checks/orchestrators/lib/verification-scopes.tsv" \
  "$REPO_ROOT/ci/checks/orchestrators/lib/named-sets.tsv" \
  "$fixture/ci/checks/orchestrators/lib/"
cp "$REPO_ROOT/ci/checks/orchestrators/case-coverage/dyn-load-prime/smoke.cases" \
  "$fixture/ci/checks/orchestrators/case-coverage/dyn-load-prime/"
# Redirect only the copied script's absolute time command. Both the capability
# probe and timed worker keep ci/all.sh's exact argv and branch structure.
# shellcheck disable=SC2016 # KIO_TEST_TIME expands in the copied script.
sed 's|/usr/bin/time|"$KIO_TEST_TIME"|g' \
  "$fixture/ci/all.sh" >"$fixture/ci/all-controlled-time.sh"

cat >"$fixture/ci/infra/sccache.sh" <<'EOF'
kio_configure_sccache_environment() {
  :
}
kio_ensure_sccache_ready() {
  :
}
EOF

cat >"$fixture/fake-corpus-job.sh" <<'EOF'
#!/bin/sh
set -eu
name=${0##*/}
name=${name%.sh}
: >"$KIO_TEST_ARG_DIR/$name.args"
for job_arg in "$@"; do
  printf '%s\n' "$job_arg" >>"$KIO_TEST_ARG_DIR/$name.args"
done
if [ "${KIO_DEBUG_SAMPLE_IMPL_SEED+x}" = x ]; then
  printf '%s\n' "$KIO_DEBUG_SAMPLE_IMPL_SEED" \
    >"$KIO_TEST_ARG_DIR/$name.impl-seed"
  printf '%s\n' "${KIO_DEBUG_SAMPLE_IMPL_SCOPE:-}" \
    >"$KIO_TEST_ARG_DIR/$name.impl-scope"
fi
EOF

awk -F '\t' 'NR > 1 { print $1 }' \
  "$REPO_ROOT/ci/checks/orchestrators/lib/orchestrator-registry.tsv" |
while IFS= read -r owner; do
  cp "$fixture/fake-corpus-job.sh" "$fixture/ci/checks/orchestrators/$owner"
done

cat >"$fixture/ci/schedule.sh" <<'EOF'
#!/bin/sh
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
scheduler_bin=${KIO_TEST_SCHEDULER_BIN:-$script_dir/../bin/fake-scheduler}
schedule_dir=${KIO_TEST_SCHEDULE_DIR:-$script_dir/../git-common/kio-ci-schedule}
[ -z "${KIO_TEST_SCHEDULE_CALLS:-}" ] ||
  printf '%s\n' "${1:-}" >>"$KIO_TEST_SCHEDULE_CALLS"
case "${1:-}" in
  --prepare) printf '%s\n' "$scheduler_bin" ;;
  --self-test) : ;;
  --readiness)
    [ "${2:-}" = -- ] && [ "$#" -gt 2 ] || exit 97
    ;;
  --available-parallelism) printf '4\n' ;;
  --state-dir)
    mkdir -p "$schedule_dir"
    printf '%s\n' "$schedule_dir"
    ;;
  *) exit 97 ;;
esac
EOF

cat >"$fixture/bin/fake-scheduler" <<'EOF'
#!/bin/sh
exit 98
EOF

# Keep the fixture portable to hosts without setsid. These workers share the
# fixture's group; the fake /proc record below supplies no signalable group.
cat >"$fixture/bin/setsid" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" != -w ] || shift
exec "$@"
EOF

cat >"$fixture/bin/gnu-time" <<'EOF'
#!/bin/sh
set -eu
if [ "$#" -eq 3 ] && [ "$1" = -f ] && [ -z "$2" ] && [ "$3" = true ]; then
  exit 0
fi
[ "${1:-}" = -o ] || exit 2
out=$2
shift 2
[ "${1:-}" = -f ] || exit 2
[ "$2" = 'KIO_CI_CPU %U %S' ] || exit 2
shift 2
set +e
"$@"
status=$?
set -e
printf 'KIO_CI_CPU 1.00 0.50\n' >"$out"
exit "$status"
EOF

cat >"$fixture/bin/non-gnu-time" <<'EOF'
#!/bin/sh
set -eu
[ "$#" -eq 3 ] && [ "$1" = -f ] && [ -z "$2" ] && [ "$3" = true ] || exit 2
exit 1
EOF

# ci/all.sh reads /proc only to record the worker process group for abort
# cleanup. A nonnumeric group prevents this fixture from issuing real group
# signals when an enclosing gate is cancelled. Delegate other sed invocations.
cat >"$fixture/bin/sed" <<'EOF'
#!/bin/sh
set -eu
last=
for arg in "$@"; do
  last=$arg
done
case "$last" in
  /proc/*/stat) printf 'S 1 fixture\n' ;;
  *) exec "$KIO_TEST_REAL_SED" "$@" ;;
esac
EOF

chmod +x \
  "$fixture/fake-corpus-job.sh" \
  "$fixture/ci/schedule.sh" \
  "$fixture/bin/fake-scheduler" \
  "$fixture/bin/setsid" \
  "$fixture/bin/sed" \
  "$fixture/bin/gnu-time" \
  "$fixture/bin/non-gnu-time"

real_sed=$(command -v sed)

gnu_cpu_summary_valid() {
  gcv_log=$1
  gcv_tasks=$2
  gcv_cpu=$3
  gcv_headers=$(grep -Ec '^========== .* \([0-9]+s wall, [0-9]+\.[0-9]s cpu, [0-9]+→[0-9]+\) ==========$' \
    "$gcv_log" || true)
  [ "$gcv_headers" -eq "$gcv_tasks" ] &&
    grep -Eq '^total wall: [0-9]+s   total cpu: [0-9]+[.][0-9]s$' "$gcv_log" &&
    grep -Fq "total cpu: ${gcv_cpu}s" "$gcv_log"
}

run_all() {
  ra_label=$1
  ra_time=$2
  ra_tasks=$3
  ra_cpu=$4
  shift 4
  ra_capability=non-gnu
  if "$ra_time" -f '' true >/dev/null 2>&1; then
    ra_capability=gnu
  fi
  LAST_ARG_DIR=$scratch/args-$ra_label
  LAST_SCHEDULE_CALLS=$scratch/schedule-$ra_label.calls
  ra_log=$scratch/$ra_label.log
  mkdir "$LAST_ARG_DIR"
  : >"$LAST_SCHEDULE_CALLS"
  if [ "${KIO_TEST_ALL_DEBUG_SAMPLE_IMPL_SEED+x}" = x ]; then
    KIO_DEBUG_SAMPLE_IMPL_SEED=$KIO_TEST_ALL_DEBUG_SAMPLE_IMPL_SEED
    export KIO_DEBUG_SAMPLE_IMPL_SEED
  else
    unset KIO_DEBUG_SAMPLE_IMPL_SEED
  fi
  unset KIO_DEBUG_SAMPLE_IMPL_SCOPE
  if ! PATH="$fixture/bin:$PATH" \
    TMPDIR="$scratch/tmp" \
    KIO_TEST_ARG_DIR="$LAST_ARG_DIR" \
    KIO_TEST_SCHEDULER_BIN="$fixture/bin/fake-scheduler" \
    KIO_TEST_SCHEDULE_DIR="$fixture/git-common/kio-ci-schedule" \
    KIO_TEST_SCHEDULE_CALLS="$LAST_SCHEDULE_CALLS" \
    KIO_TEST_REAL_SED="$real_sed" \
    KIO_TEST_TIME="$ra_time" \
    sh "$fixture/ci/all-controlled-time.sh" "$@" >"$ra_log" 2>&1; then
    cat "$ra_log" >&2
    printf 'ci-all-case-forwarding-selftest: %s fixture failed\n' \
      "$ra_label" >&2
    exit 1
  fi
  if [ "$(grep -Fxc -- --prepare "$LAST_SCHEDULE_CALLS" || :)" -ne 1 ]; then
    cat "$LAST_SCHEDULE_CALLS" >&2
    printf 'ci-all-case-forwarding-selftest: %s did not prepare the scheduler exactly once\n' \
      "$ra_label" >&2
    exit 1
  fi
  if ! grep -Eq ': DONE pass( |$)' "$ra_log"; then
    cat "$ra_log" >&2
    printf 'ci-all-case-forwarding-selftest: %s lacked a passing terminal verdict\n' \
      "$ra_label" >&2
    exit 1
  fi
  case "$ra_capability" in
    gnu)
      if ! gnu_cpu_summary_valid "$ra_log" "$ra_tasks" "$ra_cpu"; then
        cat "$ra_log" >&2
        printf 'ci-all-case-forwarding-selftest: %s GNU CPU summary is invalid\n' \
          "$ra_label" >&2
        exit 1
      fi
      ;;
    non-gnu)
      ra_headers=$(grep -Ec '^========== .* \([0-9]+s wall, -s cpu, [0-9]+→[0-9]+\) ==========$' \
        "$ra_log" || true)
      total_re='^total wall: [0-9]+s$'
      if [ "$ra_headers" -ne "$ra_tasks" ] ||
         ! grep -Eq "$total_re" "$ra_log" ||
         grep -Fq 'total cpu:' "$ra_log"; then
        cat "$ra_log" >&2
        printf 'ci-all-case-forwarding-selftest: %s non-GNU CPU summary is invalid\n' \
          "$ra_label" >&2
        exit 1
      fi
      ;;
  esac
}

assert_args() {
  aa_file=$1
  shift
  aa_expected=$scratch/expected.args
  : >"$aa_expected"
  for aa_arg in "$@"; do
    printf '%s\n' "$aa_arg" >>"$aa_expected"
  done
  if [ ! -f "$aa_file" ] || ! diff -u "$aa_expected" "$aa_file"; then
    printf 'ci-all-case-forwarding-selftest: wrong forwarded argv in %s\n' \
      "$aa_file" >&2
    exit 1
  fi
}

# Pin the actual hosted-gate branch as one exact block, so unrelated mentions
# elsewhere in the workflow cannot make a missing sampled flag look green.
awk '
  /^          if \[ "\$CASE_COVERAGE" = full \]; then$/ { capture=1 }
  capture {
    end = ($0 == "          fi")
    sub(/^          /, "")
    print
    if (end) exit
  }
' "$WORKFLOW" >"$scratch/actual.workflow-coverage"
cat >"$scratch/expected.workflow-coverage" <<'EOF'
if [ "$CASE_COVERAGE" = full ]; then
  sh ci/all.sh "$IMPLS" --all-cases
else
  sh ci/all.sh "$IMPLS" --sample-cases \
    --impl-verification=dyn-load-prime \
    --case-coverage=dyn-load-prime:10
fi
EOF
if ! diff -u "$scratch/expected.workflow-coverage" \
    "$scratch/actual.workflow-coverage"; then
  printf 'ci-all-case-forwarding-selftest: GitHub case-coverage commands drifted\n' >&2
  exit 1
fi

# Drive the workflow's real implementation and case decisions before feeding
# its argv through the real aggregate dispatcher and fake corpus jobs.
workflow_fixture=$scratch/workflow
mkdir -p "$workflow_fixture/ci"
cp "$REPO_ROOT/ci/impl-toolchain.sh" "$workflow_fixture/ci/"
awk '
  /^        id: impls$/ { step = 1; next }
  step && /^        run: \|$/ { body = 1; next }
  body && /^          / { sub(/^          /, ""); print; next }
  body { exit }
' "$WORKFLOW" >"$scratch/workflow-impls.sh"
cat >"$workflow_fixture/ci/all.sh" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" >"$KIO_TEST_WORKFLOW_ARGS"
EOF
workflow_default=$(awk '
  /^      implementation_coverage:$/ { input = 1; next }
  input && /^        default:/ { print $2; exit }
' "$WORKFLOW")
if [ "$workflow_default" != sampled ]; then
  printf 'ci-all-case-forwarding-selftest: hosted implementation default must be sampled\n' >&2
  exit 1
fi
# shellcheck disable=SC2016 # Match the workflow expression literally.
grep -Fq "IMPLEMENTATION_COVERAGE: \${{ inputs.implementation_coverage || 'sampled' }}" "$WORKFLOW"

for workflow_impl_policy in "$workflow_default" full; do
  : >"$scratch/workflow.outputs"
  (
    cd "$workflow_fixture"
    IMPLEMENTATION_COVERAGE="$workflow_impl_policy" \
      GITHUB_RUN_ID=2 GITHUB_RUN_ATTEMPT=1 \
      GITHUB_OUTPUT="$scratch/workflow.outputs" \
      sh -eu "$scratch/workflow-impls.sh" >"$scratch/workflow-selection.log"
  )
  workflow_impls=$(sed -n 's/^impls=//p' "$scratch/workflow.outputs")
  workflow_tools=$(sed -n 's/^tool_impls=//p' "$scratch/workflow.outputs")
  expected_impls=SAMPLE_IMPL
  [ "$workflow_impl_policy" != full ] || expected_impls=FULL_IMPL_MATRIX
  if [ "$workflow_impls" != "$expected_impls" ] ||
     [ "$workflow_tools" != "$(sh "$REPO_ROOT/ci/impl-toolchain.sh" impls)" ]; then
    cat "$scratch/workflow.outputs" >&2
    printf 'ci-all-case-forwarding-selftest: wrong hosted implementation mode or incomplete tool pool\n' >&2
    exit 1
  fi
  for workflow_case_policy in sampled full; do
    (
      cd "$workflow_fixture"
      IMPLS="$workflow_impls" CASE_COVERAGE="$workflow_case_policy" \
        KIO_TEST_WORKFLOW_ARGS="$scratch/workflow.args" \
        sh -eu "$scratch/actual.workflow-coverage"
    )
    set --
    while IFS= read -r workflow_arg; do set -- "$@" "$workflow_arg"; done <"$scratch/workflow.args"
    run_all "workflow-$workflow_impl_policy-$workflow_case_policy" \
      "$fixture/bin/gnu-time" 20 30.0 "$@" --case-seed=fixed \
      --jobs=1 --compiler-jobs=1 --no-live
    case_flag=--sample-cases
    [ "$workflow_case_policy" != full ] || case_flag=--all-cases
    for owner in emissions poc castle contrib; do
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        "--impls=$expected_impls" "$case_flag" --case-seed=fixed
    done
    set -- "--impls=$expected_impls" "$case_flag" --case-seed=fixed
    [ "$workflow_case_policy" != sampled ] ||
      set -- "$@" --case-coverage=dyn-load-prime:10
    assert_args "$LAST_ARG_DIR/golden-tests.args" "$@"
    assert_args "$LAST_ARG_DIR/generative-tests.args" "--impls=$expected_impls"
  done
done

# Execute the portability step's real shell body with fake Cargo and harness
# entry points. This checks the shared macOS/Windows case policy without
# compiling a package or walking the golden corpus.
awk '
  /^      - name: Golden compile checks / { step = 1; next }
  step && /^        run: \|$/ { body = 1; next }
  body && /^          / { sub(/^          /, ""); print; next }
  body { exit }
' "$WORKFLOW" >"$scratch/workflow-portability.sh"
awk '
  /^      - name: Golden compile checks / { step = 1; next }
  step && /^        run: \|$/ { exit }
  step && /^          CASE_COVERAGE:/ { print; exit }
' "$WORKFLOW" >"$scratch/actual.portability-env"
cat >"$scratch/expected.portability-env" <<'EOF'
          CASE_COVERAGE: ${{ inputs.case_coverage || 'sampled' }}
EOF
if ! diff -u "$scratch/expected.portability-env" \
    "$scratch/actual.portability-env"; then
  printf 'ci-all-case-forwarding-selftest: portability case default drifted\n' >&2
  exit 1
fi

cat >"$workflow_fixture/ci/cargo.sh" <<'EOF'
#!/bin/sh
printf 'call\n' >>"$KIO_TEST_PORTABILITY_CARGO_CALLS"
EOF
cat >"$workflow_fixture/ci/run-tests.sh" <<'EOF'
#!/bin/sh
printf 'call\n' >>"$KIO_TEST_PORTABILITY_HARNESS_CALLS"
printf '%s\n' "$@" >"$KIO_TEST_PORTABILITY_ARGS"
EOF
for portability_policy in sampled full invalid; do
  : >"$scratch/portability.cargo-calls"
  : >"$scratch/portability.harness-calls"
  : >"$scratch/portability.args"
  if (
    cd "$workflow_fixture"
    CASE_COVERAGE="$portability_policy" TMPDIR="$scratch/tmp" \
      KIO_TEST_PORTABILITY_CARGO_CALLS="$scratch/portability.cargo-calls" \
      KIO_TEST_PORTABILITY_HARNESS_CALLS="$scratch/portability.harness-calls" \
      KIO_TEST_PORTABILITY_ARGS="$scratch/portability.args" \
      sh -eu "$scratch/workflow-portability.sh"
  ) >"$scratch/portability.log" 2>&1; then
    portability_status=0
  else
    portability_status=$?
  fi
  if [ "$portability_policy" = invalid ]; then
    if [ "$portability_status" != 2 ] ||
       [ -s "$scratch/portability.cargo-calls" ] ||
       [ -s "$scratch/portability.harness-calls" ]; then
      printf 'ci-all-case-forwarding-selftest: invalid portability policy reached a command\n' >&2
      exit 1
    fi
    continue
  fi
  if [ "$portability_status" != 0 ] ||
     [ "$(wc -l <"$scratch/portability.cargo-calls")" -ne 1 ] ||
     [ "$(wc -l <"$scratch/portability.harness-calls")" -ne 1 ]; then
    printf 'ci-all-case-forwarding-selftest: portability step did not run exactly once\n' >&2
    cat "$scratch/portability.log" >&2
    exit 1
  fi
  portability_cache=$(sed -n 's/^--cache-base=//p' "$scratch/portability.args")
  case "$portability_cache" in
    "$scratch"/tmp/tmp.*/cache) ;;
    *) printf 'ci-all-case-forwarding-selftest: portability cache path drifted\n' >&2; exit 1 ;;
  esac
  sed 's|^--cache-base=.*|--cache-base=<work>/cache|' \
    "$scratch/portability.args" >"$scratch/portability.normalized.args"
  portability_count=100
  [ "$portability_policy" != full ] || portability_count=all
  assert_args "$scratch/portability.normalized.args" \
    --cases-dir=test-data/goldens/00_success \
    '--cache-base=<work>/cache' \
    "--impl-def=name=kio,kio=$workflow_fixture/kio-rs/target/debug/kio,runner=SKIP,target=js" \
    "--sample-cases=$portability_count" --keep-cache
done

run_all github-sampled "$fixture/bin/gnu-time" 20 30.0 \
  kio@js --sample-cases --impl-verification=dyn-load-prime \
  --case-coverage=dyn-load-prime:10 --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=kio@js,dyn-load-prime@kio-prime --sample-cases \
  --case-seed=fixed --case-coverage=dyn-load-prime:10
for corpus in emissions poc castle contrib; do
  assert_args "$LAST_ARG_DIR/$corpus-tests.args" \
    --impls=kio@js --sample-cases --case-seed=fixed
done
assert_args "$LAST_ARG_DIR/generative-tests.args" --impls=kio@js
# A complete null-root owner remains a normal aggregate task but receives no
# implementation, case, verification, or seed arguments.
assert_args "$LAST_ARG_DIR/builtin-docs.args"

# Registry and routing scratch falls back to the fixture's ignored target
# directory, not the caller's current directory, when TMPDIR is absent.
arbitrary_arg_dir=$scratch/args-arbitrary-cwd
arbitrary_logs=$scratch/logs-arbitrary-cwd
arbitrary_log=$scratch/arbitrary-cwd.log
mkdir "$arbitrary_arg_dir"
if ! (
  unset TMPDIR
  cd /
  PATH="$fixture/bin:$PATH" \
    KIO_TEST_ARG_DIR="$arbitrary_arg_dir" \
    KIO_TEST_REAL_SED="$real_sed" \
    KIO_TEST_TIME="$fixture/bin/gnu-time" \
    sh "$fixture/ci/all-controlled-time.sh" \
      kio@js --sample-cases --case-seed=fixed \
      --jobs=1 --compiler-jobs=1 --no-live \
      "--keep-logs=$arbitrary_logs"
) >"$arbitrary_log" 2>&1; then
  cat "$arbitrary_log" >&2
  printf 'ci-all-case-forwarding-selftest: arbitrary-CWD fixture failed\n' >&2
  exit 1
fi
if ! grep -Eq ': DONE pass( |$)' "$arbitrary_log" ||
   directory_has_entry "$fixture/target"; then
  cat "$arbitrary_log" >&2
  printf 'ci-all-case-forwarding-selftest: arbitrary-CWD scratch leaked or lacked a passing verdict\n' >&2
  exit 1
fi
assert_args "$arbitrary_arg_dir/golden-tests.args" \
  --impls=kio@js --sample-cases --case-seed=fixed

# Mutate the copied aggregate helper to read only the first CPU record. The
# broad wrapper still completes, but the exact 30.0-second oracle
# must go red at 1.5 seconds; restoring the helper must restore the oracle.
cpu_sum_canonical=$scratch/ci-time-cpu-sum.canonical
cp "$fixture/ci/infra/ci-time-cpu-sum.sh" "$cpu_sum_canonical"
# shellcheck disable=SC2016 # Mutate the copied script's literal "$@" to "$1".
sed 's/"$@"/"$1"/' "$cpu_sum_canonical" \
  >"$fixture/ci/infra/ci-time-cpu-sum.sh"
mutant_arg_dir=$scratch/args-cpu-single-file-mutant
mutant_log=$scratch/cpu-single-file-mutant.log
mkdir "$mutant_arg_dir"
if ! PATH="$fixture/bin:$PATH" \
  TMPDIR="$scratch/tmp" \
  KIO_TEST_ARG_DIR="$mutant_arg_dir" \
  KIO_TEST_REAL_SED="$real_sed" \
  KIO_TEST_TIME="$fixture/bin/gnu-time" \
  sh "$fixture/ci/all-controlled-time.sh" \
    kio@js --sample-cases --case-seed=fixed \
    --jobs=1 --compiler-jobs=1 --no-live >"$mutant_log" 2>&1; then
  cat "$mutant_log" >&2
  printf 'ci-all-case-forwarding-selftest: CPU aggregate mutant failed to run\n' >&2
  exit 1
fi
if gnu_cpu_summary_valid "$mutant_log" 20 30.0 ||
   ! grep -Eq '^total wall: [0-9]+s   total cpu: 1[.]5s$' "$mutant_log"; then
  cat "$mutant_log" >&2
  printf 'ci-all-case-forwarding-selftest: single-file CPU aggregate mutant did not go red\n' >&2
  exit 1
fi
cp "$cpu_sum_canonical" "$fixture/ci/infra/ci-time-cpu-sum.sh"
if ! cmp -s "$cpu_sum_canonical" "$fixture/ci/infra/ci-time-cpu-sum.sh"; then
  printf 'ci-all-case-forwarding-selftest: CPU aggregate helper was not restored\n' >&2
  exit 1
fi
run_all cpu-aggregate-restored "$fixture/bin/gnu-time" 20 30.0 \
  kio@js --sample-cases --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live

# The same ci/all.sh probe selects the portable no-CPU summary when GNU time
# formatting is unavailable. Case forwarding remains identical.
run_all non-gnu-time "$fixture/bin/non-gnu-time" 20 30.0 \
  kio@js --sample-cases --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/emissions-tests.args" \
  --impls=kio@js --sample-cases --case-seed=fixed

# A per-corpus numeric override wins over the global all-cases choice.
run_all all-with-emissions-sample "$fixture/bin/gnu-time" 20 30.0 \
  FULL_IMPL_MATRIX --all-cases --case-coverage=emissions:1 --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=FULL_IMPL_MATRIX --all-cases --case-seed=fixed
assert_args "$LAST_ARG_DIR/emissions-tests.args" \
  --impls=FULL_IMPL_MATRIX --case-count=1 --case-seed=fixed

# The inverse override is equally important: emissions remain exhaustive when
# every other corpus is sampled.
run_all sample-with-all-emissions "$fixture/bin/gnu-time" 20 30.0 \
  SAMPLE_IMPL --sample-cases --case-coverage=emissions:all --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=SAMPLE_IMPL --sample-cases --case-seed=fixed
assert_args "$LAST_ARG_DIR/emissions-tests.args" \
  --impls=SAMPLE_IMPL --all-cases --case-seed=fixed

# Explicit regular implementations remain byte-for-byte the same set for all
# matrix owners when no fixed verifier is requested.
run_all explicit-baseline "$fixture/bin/gnu-time" 20 30.0 \
  kio@js,kio@rust --sample-cases --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
for owner in golden emissions generative poc castle contrib; do
  case $owner in
    generative)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@js,kio@rust ;;
    *)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@js,kio@rust --sample-cases --case-seed=fixed ;;
  esac
done

# Preserve caller order as well as membership; a registry union must not sort
# or otherwise rewrite an unrelated explicit implementation list.
run_all explicit-reverse-order "$fixture/bin/gnu-time" 20 30.0 \
  kio@rust,kio@js --sample-cases --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
for owner in golden emissions generative poc castle contrib; do
  case $owner in
    generative)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@rust,kio@js ;;
    *)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@rust,kio@js --sample-cases --case-seed=fixed ;;
  esac
done

# Ordinary duplicates are caller-owned bytes, not fixed-verifier duplicates;
# every matrix owner must receive them unchanged.
run_all explicit-duplicate "$fixture/bin/gnu-time" 20 30.0 \
  kio@js,kio@js,kio@rust --sample-cases --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
for owner in golden emissions generative poc castle contrib; do
  case $owner in
    generative)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@js,kio@js,kio@rust ;;
    *)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@js,kio@js,kio@rust --sample-cases --case-seed=fixed ;;
  esac
done

# Augmentation preserves the same duplicate regular list while adding one
# owner-private fixed row exactly once.
run_all explicit-duplicate-augment "$fixture/bin/gnu-time" 20 30.0 \
  kio@js,kio@js,kio@rust --sample-cases \
  --impl-verification=dyn-load-prime --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=kio@js,kio@js,kio@rust,dyn-load-prime@kio-prime \
  --sample-cases --case-seed=fixed
for owner in emissions generative poc castle contrib; do
  case $owner in
    generative)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@js,kio@js,kio@rust ;;
    *)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@js,kio@js,kio@rust --sample-cases --case-seed=fixed ;;
  esac
done

# A requested fixed verifier augments only its owner, after the unchanged
# regular list. Repeating the same request is an exact no-op.
for augment_label in explicit-augment repeat-augment; do
  case $augment_label in
    explicit-augment) augment_args='--impl-verification=dyn-load-prime' ;;
    repeat-augment)
      augment_args='--impl-verification=dyn-load-prime --impl-verification=dyn-load-prime'
      ;;
  esac
  # shellcheck disable=SC2086 # fixture deliberately expands one or two args.
  run_all "$augment_label" "$fixture/bin/gnu-time" 20 30.0 \
    kio@rust,kio@js --sample-cases $augment_args --case-seed=fixed \
    --jobs=1 --compiler-jobs=1 --no-live
  assert_args "$LAST_ARG_DIR/golden-tests.args" \
    --impls=kio@rust,kio@js,dyn-load-prime@kio-prime --sample-cases \
    --case-seed=fixed
  for owner in emissions generative poc castle contrib; do
    case $owner in
      generative)
        assert_args "$LAST_ARG_DIR/$owner-tests.args" \
          --impls=kio@rust,kio@js ;;
      *)
        assert_args "$LAST_ARG_DIR/$owner-tests.args" \
          --impls=kio@rust,kio@js --sample-cases --case-seed=fixed ;;
    esac
  done
done

# Repeated activation and an already-selected fixed name both deduplicate.
run_all repeat-fixed "$fixture/bin/gnu-time" 20 30.0 \
  kio@js,dyn-load-prime@kio-prime --sample-cases \
  --impl-verification=dyn-load-prime \
  --impl-verification=dyn-load-prime --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=kio@js,dyn-load-prime@kio-prime --sample-cases \
  --case-seed=fixed
for owner in emissions generative poc castle contrib; do
  case $owner in
    generative)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" --impls=kio@js ;;
    *)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=kio@js --sample-cases --case-seed=fixed ;;
  esac
done

# SAMPLE_IMPL already retains every fixed verification group; requesting one
# cannot add a second copy or perturb any owner's ordinary sample mode.
run_all sample-dedupe "$fixture/bin/gnu-time" 20 30.0 \
  SAMPLE_IMPL --impl-verification=dyn-load-prime --sample-cases \
  --case-seed=fixed --jobs=1 --compiler-jobs=1 --no-live
for owner in golden emissions generative poc castle contrib; do
  case $owner in
    generative)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" --impls=SAMPLE_IMPL ;;
    *)
      assert_args "$LAST_ARG_DIR/$owner-tests.args" \
        --impls=SAMPLE_IMPL --sample-cases --case-seed=fixed ;;
  esac
done

# The debug replay seed is validated once by ci/all.sh and reaches only every
# SAMPLE_IMPL corpus task with an exact task scope. Selection-domain behavior
# is covered by run-tests-predispatch-selftest; this fixture proves forwarding.
KIO_TEST_ALL_DEBUG_SAMPLE_IMPL_SEED=4294967295 \
  run_all sample-debug-seed "$fixture/bin/gnu-time" 20 30.0 \
    SAMPLE_IMPL --sample-cases --case-seed=fixed \
    --jobs=1 --compiler-jobs=1 --no-live
unset KIO_TEST_ALL_DEBUG_SAMPLE_IMPL_SEED
for owner in golden emissions generative poc castle contrib; do
  if [ "$(cat "$LAST_ARG_DIR/$owner-tests.impl-seed")" != 4294967295 ] ||
     [ "$(cat "$LAST_ARG_DIR/$owner-tests.impl-scope")" != \
       "checks/orchestrators/$owner-tests" ]; then
    printf 'ci-all-case-forwarding-selftest: debug SAMPLE_IMPL seed/scope did not reach %s exactly\n' \
      "$owner" >&2
    exit 1
  fi
done
seeded_task_count=$(find "$LAST_ARG_DIR" -name '*.impl-seed' -type f | wc -l | tr -d ' ')
if [ "$seeded_task_count" -ne 6 ]; then
  find "$LAST_ARG_DIR" -name '*.impl-seed' -type f -print >&2
  printf 'ci-all-case-forwarding-selftest: debug SAMPLE_IMPL seed leaked beyond the six corpus owners\n' >&2
  exit 1
fi

invalid_seed_args=$scratch/args-invalid-debug-seed
invalid_seed_log=$scratch/invalid-debug-seed.log
mkdir "$invalid_seed_args"
set +e
PATH="$fixture/bin:$PATH" \
  TMPDIR="$scratch/tmp" \
  KIO_DEBUG_SAMPLE_IMPL_SEED=01 \
  KIO_TEST_ARG_DIR="$invalid_seed_args" \
  KIO_TEST_SCHEDULER_BIN="$fixture/bin/fake-scheduler" \
  KIO_TEST_SCHEDULE_DIR="$fixture/git-common/kio-ci-schedule" \
  KIO_TEST_REAL_SED="$real_sed" \
  KIO_TEST_TIME="$fixture/bin/gnu-time" \
  sh "$fixture/ci/all-controlled-time.sh" SAMPLE_IMPL --no-live \
    >"$invalid_seed_log" 2>&1
invalid_seed_status=$?
set -e
if [ "$invalid_seed_status" -ne 2 ] ||
   directory_has_entry "$invalid_seed_args" ||
   ! grep -Fq 'KIO_DEBUG_SAMPLE_IMPL_SEED must be canonical decimal in 0..4294967295' \
     "$invalid_seed_log"; then
  cat "$invalid_seed_log" >&2
  printf 'ci-all-case-forwarding-selftest: invalid debug SAMPLE_IMPL seed did not fail before task dispatch\n' >&2
  exit 1
fi

wrong_mode_seed_args=$scratch/args-wrong-mode-debug-seed
wrong_mode_seed_log=$scratch/wrong-mode-debug-seed.log
mkdir "$wrong_mode_seed_args"
set +e
PATH="$fixture/bin:$PATH" \
  TMPDIR="$scratch/tmp" \
  KIO_DEBUG_SAMPLE_IMPL_SEED=0 \
  KIO_TEST_ARG_DIR="$wrong_mode_seed_args" \
  KIO_TEST_SCHEDULER_BIN="$fixture/bin/fake-scheduler" \
  KIO_TEST_SCHEDULE_DIR="$fixture/git-common/kio-ci-schedule" \
  KIO_TEST_REAL_SED="$real_sed" \
  KIO_TEST_TIME="$fixture/bin/gnu-time" \
  sh "$fixture/ci/all-controlled-time.sh" FULL_IMPL_MATRIX --no-live \
    >"$wrong_mode_seed_log" 2>&1
wrong_mode_seed_status=$?
set -e
if [ "$wrong_mode_seed_status" -ne 2 ] ||
   directory_has_entry "$wrong_mode_seed_args" ||
   ! grep -Fq 'KIO_DEBUG_SAMPLE_IMPL_SEED requires SAMPLE_IMPL coverage' \
     "$wrong_mode_seed_log"; then
  cat "$wrong_mode_seed_log" >&2
  printf 'ci-all-case-forwarding-selftest: debug implementation seed without SAMPLE_IMPL did not fail before task dispatch\n' >&2
  exit 1
fi

# A private fixed implementation routes only to its owner. Other
# implementation-matrix jobs receive an empty set and do not launch; null-root
# checks remain part of the aggregate gate.
run_all fixed-owner-only "$fixture/bin/gnu-time" 15 22.5 \
  dyn-load-prime@kio-prime --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=dyn-load-prime@kio-prime --case-seed=fixed
for owner in emissions generative poc castle contrib; do
  if [ -e "$LAST_ARG_DIR/$owner-tests.args" ]; then
    printf 'ci-all-case-forwarding-selftest: private fixed impl reached %s owner\n' \
      "$owner" >&2
    exit 1
  fi
done

# Root policy maps to the owner's direct spelling; leaf policy stays scoped.
run_all root-sample "$fixture/bin/gnu-time" 20 30.0 \
  kio@js --all-cases --case-coverage goldens:sample \
  --impl-verification dyn-load-prime \
  --case-coverage=dyn-load-prime:smoke --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=kio@js,dyn-load-prime@kio-prime --sample-cases \
  --case-seed=fixed --case-coverage=dyn-load-prime:smoke

run_all root-numeric "$fixture/bin/gnu-time" 20 30.0 \
  kio@js --sample-cases --case-coverage=goldens:7 --case-seed=fixed \
  --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=kio@js --case-count=7 --case-seed=fixed

run_all local-default "$fixture/bin/gnu-time" 20 30.0 \
  SAMPLE_IMPL --case-seed=fixed --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=SAMPLE_IMPL --case-seed=fixed

for full_order in first last; do
  case $full_order in
    first) set -- --case-coverage=dyn-load-prime:all --sample-cases ;;
    last) set -- --sample-cases --case-coverage=dyn-load-prime:all ;;
  esac
  run_all "leaf-full-$full_order" "$fixture/bin/gnu-time" 20 30.0 \
    SAMPLE_IMPL "$@" --case-seed=fixed --jobs=1 --compiler-jobs=1 --no-live
  assert_args "$LAST_ARG_DIR/golden-tests.args" \
    --impls=SAMPLE_IMPL --sample-cases --case-seed=fixed \
    --case-coverage=dyn-load-prime:all
done

run_all root-full "$fixture/bin/gnu-time" 20 30.0 \
  SAMPLE_IMPL --sample-cases --case-coverage=goldens:all \
  --case-seed=fixed --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=SAMPLE_IMPL --all-cases --case-seed=fixed

run_all full-dedupe "$fixture/bin/gnu-time" 20 30.0 \
  FULL_IMPL_MATRIX --impl-verification=dyn-load-prime --all-cases \
  --case-seed=fixed --jobs=1 --compiler-jobs=1 --no-live
assert_args "$LAST_ARG_DIR/golden-tests.args" \
  --impls=FULL_IMPL_MATRIX --all-cases --case-seed=fixed

run_all_rejected() {
  rar_label=$1
  rar_pattern=$2
  shift 2
  rar_args=$scratch/args-rejected-$rar_label
  rar_log=$scratch/rejected-$rar_label.log
  mkdir "$rar_args"
  set +e
  PATH="$fixture/bin:$PATH" \
    TMPDIR="$scratch/tmp" \
    KIO_TEST_ARG_DIR="$rar_args" \
    KIO_TEST_REAL_SED="$real_sed" \
    KIO_TEST_TIME="$fixture/bin/gnu-time" \
    sh "$fixture/ci/all-controlled-time.sh" "$@" >"$rar_log" 2>&1
  rar_status=$?
  set -e
  if [ "$rar_status" -ne 2 ] || ! grep -Fq -- "$rar_pattern" "$rar_log" ||
     directory_has_entry "$rar_args"; then
    cat "$rar_log" >&2
    printf 'ci-all-case-forwarding-selftest: invalid policy reached task launch: %s\n' \
      "$rar_label" >&2
    exit 1
  fi
}

run_all_rejected unknown-verification 'unknown implementation-verification scope' \
  kio@js --impl-verification=does-not-exist
run_all_rejected root-as-verification 'requires a fixed-verification scope' \
  kio@js --impl-verification=goldens
run_all_rejected inactive-leaf 'targets inactive verification scope dyn-load-prime' \
  kio@js --case-coverage=dyn-load-prime:smoke
run_all_rejected unknown-named 'unknown named case policy no-such-set' \
  kio@js --impl-verification=dyn-load-prime \
  --case-coverage=dyn-load-prime:no-such-set
run_all_rejected zero-policy 'invalid case-coverage policy: 0' \
  kio@js --case-coverage=goldens:0
run_all_rejected padded-zero-policy 'invalid case-coverage policy: 00' \
  kio@js --case-coverage=goldens:00
run_all_rejected empty-verification '--impl-verification requires a value' \
  kio@js --impl-verification=
run_all_rejected empty-coverage '--case-coverage requires a value' \
  kio@js --case-coverage=
run_all_rejected old-migration \
  'use --case-coverage=emissions:1' kio@js --emissions=1

printf 'ci-all-case-forwarding-selftest: ok (registry routing; exact 20-task CPU 30.0s; single-file 1.5s mutant rejected and restored)\n'
