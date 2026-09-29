#!/bin/sh

# Keep the live heartbeat's implementation, checks-only, and total denominators
# aligned with effective case sampling and whole-corpus case-binary work.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
RUN_TESTS_SH=${RUN_TESTS_SH:-"$REPO_ROOT/ci/run-tests.sh"}
if [ -z "${KIO_CI_SCHEDULER_BIN:-}" ]; then
  KIO_CI_SCHEDULER_BIN=$(sh "$REPO_ROOT/ci/schedule.sh" --prepare)
  export KIO_CI_SCHEDULER_BIN
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

mkdir -p "$scratch/bin" "$scratch/cases/bucket" "$scratch/tmp" "$scratch/cache"

cat >"$scratch/bin/kio" <<'EOF'
#!/bin/sh
[ -z "${KIO_TEST_KIO_MARKER:-}" ] || : >"$KIO_TEST_KIO_MARKER"
if [ -n "${KIO_TEST_TYPED_ROOT_MARKER:-}" ]; then
  printf '%s\n' "${KIO_DEBUG_TYPED_CACHE_ROOT:-}" >>"$KIO_TEST_TYPED_ROOT_MARKER"
  [ -z "${KIO_DEBUG_TYPED_CACHE_ROOT:-}" ] || mkdir -p "$KIO_DEBUG_TYPED_CACHE_ROOT"
fi
case "${1:-} ${2:-}" in
  'cache --help') exit 0 ;;
  'cache clear')
    [ "${KIO_TEST_FAST_CACHE:-0}" = 1 ] || sleep 2
    exit 0
    ;;
esac
exit 0
EOF

cat >"$scratch/bin/sccache" <<'EOF'
#!/bin/sh
case "${1:-}" in
  --dist-status) exit 1 ;;
esac
exit 97
EOF

cat >"$scratch/bin/runner" <<'EOF'
#!/bin/sh
exit 0
EOF

for name in one two three four; do
  case_dir=$scratch/cases/bucket/$name
  mkdir -p "$case_dir"
  cat >"$case_dir/run.sh" <<'EOF'
#!/bin/sh
if [ "${KIO_TEST_REQUIRE_WORK:-0}" = 1 ]; then
  case ",${KIO_CI_SCHEDULE_HELD:-}," in
    *,work,*) ;;
    *) exit 99 ;;
  esac
fi
[ -z "${KIO_TEST_RUN_SH_DELAY_SECONDS:-}" ] || \
  sleep "$KIO_TEST_RUN_SH_DELAY_SECONDS"
if [ -n "${KIO_TEST_EXPECT_JOBS:-}" ] &&
   [ "${KIO_CI_SCHEDULE_JOBS:-}" != "$KIO_TEST_EXPECT_JOBS" ]; then
  exit 98
fi
if [ -n "${KIO_TEST_RUN_SH_TYPED_ROOT_MARKER:-}" ]; then
  printf '%s\t%s\n' "$PWD" "${KIO_DEBUG_TYPED_CACHE_ROOT:-}" \
    >>"$KIO_TEST_RUN_SH_TYPED_ROOT_MARKER"
  [ -z "${KIO_DEBUG_TYPED_CACHE_ROOT:-}" ] || mkdir -p "$KIO_DEBUG_TYPED_CACHE_ROOT"
fi
if env | grep -q '^KIO_TEST_WORKER_'; then
  exit 95
fi
if [ -n "${KIO_TEST_MUTATE_FROM_CASE:-}" ] &&
   [ "$PWD" = "$KIO_TEST_MUTATE_FROM_CASE" ] &&
   [ -n "${KIO_TEST_MUTATE_RUN_SH:-}" ]; then
  printf '#!/bin/sh\nexit 97\n' >"$KIO_TEST_MUTATE_RUN_SH"
fi
if [ -n "${KIO_TEST_MUTATE_FROM_CASE:-}" ] &&
   [ "$PWD" = "$KIO_TEST_MUTATE_FROM_CASE" ] &&
   [ -n "${KIO_TEST_MUTATE_AUTHENTICATED_SNAPSHOT:-}" ]; then
  run_root=${KIO_DEBUG_TYPED_CACHE_ROOT%/custom-typed-cache}
  master_snapshot=$run_root/custom-typed-cache-scripts/$KIO_TEST_MUTATE_AUTHENTICATED_SNAPSHOT
  chmod u+w "$master_snapshot"
  printf '#!/bin/sh\nexit 96\n' >"$master_snapshot"
fi
exit 0
EOF
  printf '0\n' >"$case_dir/expected.exit"
  : >"$case_dir/expected.stdout"
  : >"$case_dir/expected.stderr.ignore"
  chmod +x "$case_dir/run.sh"
done
chmod +x "$scratch/bin/kio" "$scratch/bin/runner" "$scratch/bin/sccache"

run_harness() {
  rh_log=$1
  shift
  rh_run_tests=${KIO_TEST_RUN_TESTS_SH:-$RUN_TESTS_SH}
  rh_cases_dir=${KIO_TEST_CASES_DIR:-$scratch/cases}
  if [ "${KIO_TEST_OMIT_JOBS:-0}" != 1 ]; then
    set -- "--jobs=${KIO_TEST_JOBS:-4}" "$@"
  fi
  KIO_CI_PROGRESS_FD='' KIO_CI_TASK_NAME='' KIO_DEBUG_PROGRESS_INTERVAL=1 \
    KIO_CI_SCHEDULE_DIR="$scratch/schedule" \
    KIO_CI_SCHEDULE_JOBS="${KIO_TEST_SCHEDULE_JOBS:-4}" \
    KIO_TEST_REQUIRE_WORK="${KIO_TEST_REQUIRE_WORK:-0}" \
    KIO_TEST_EXPECT_JOBS="${KIO_TEST_EXPECT_JOBS:-}" \
    KIO_TEST_FAST_CACHE="${KIO_TEST_FAST_CACHE:-0}" \
    KIO_TEST_KIO_MARKER="${KIO_TEST_KIO_MARKER:-}" \
    KIO_TEST_RUN_SH_DELAY_SECONDS="${KIO_TEST_RUN_SH_DELAY_SECONDS:-}" \
    KIO_TEST_TYPED_ROOT_MARKER="${KIO_TEST_TYPED_ROOT_MARKER:-}" \
    KIO_TEST_RUN_SH_TYPED_ROOT_MARKER="${KIO_TEST_RUN_SH_TYPED_ROOT_MARKER:-}" \
    KIO_TEST_MUTATE_FROM_CASE="${KIO_TEST_MUTATE_FROM_CASE:-}" \
    KIO_TEST_MUTATE_RUN_SH="${KIO_TEST_MUTATE_RUN_SH:-}" \
    KIO_TEST_MUTATE_AUTHENTICATED_SNAPSHOT="${KIO_TEST_MUTATE_AUTHENTICATED_SNAPSHOT:-}" \
    KIO_TEST_RUNNER_COMPILER_WRAPPER="${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}" \
    RUSTC_WRAPPER="${RUSTC_WRAPPER:-}" \
    RUSTC_WORKSPACE_WRAPPER="${RUSTC_WORKSPACE_WRAPPER:-}" \
    CARGO_BUILD_RUSTC_WRAPPER="${CARGO_BUILD_RUSTC_WRAPPER:-}" \
    CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER="${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" \
    TMPDIR="$scratch/tmp" sh "$rh_run_tests" \
    --cases-dir="$rh_cases_dir" \
    --cache-base="$scratch/cache" \
    --impl-def="name=fixture,kio=$scratch/bin/kio,runner=$scratch/bin/runner,target=js" \
    "$@" >"$rh_log" 2>&1
}

set +e
RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER="$scratch/bin/sccache" \
  KIO_TEST_KIO_MARKER="$scratch/kio-ran" \
  run_harness "$scratch/sccache-failure.log" --sample-cases=1 --case-seed=fixed
sccache_status=$?
set -e
if [ "$sccache_status" -ne 2 ] ||
   ! grep -q '^error: could not contact configured sccache wrapper ' \
     "$scratch/sccache-failure.log" ||
   [ -e "$scratch/kio-ran" ]; then
  cat "$scratch/sccache-failure.log" >&2
  printf 'run-tests-progress-selftest: cache-readiness failure did not stop before runner work\n' >&2
  exit 1
fi

if ! KIO_TEST_RUN_SH_DELAY_SECONDS=5 \
  run_harness "$scratch/sample.log" --keep-cache \
    --sample-cases=1 --case-seed=fixed; then
  cat "$scratch/sample.log" >&2
  printf 'run-tests-progress-selftest: sampled fixture failed\n' >&2
  exit 1
fi
if ! grep -Eq 'progress: 0/1 impl-run units done; 3/3 checks-only units done; 3/4 total units done,' \
  "$scratch/sample.log"; then
  cat "$scratch/sample.log" >&2
  printf 'run-tests-progress-selftest: sampled heartbeat did not report its implementation, checks-only, and total denominators\n' >&2
  exit 1
fi

if ! run_harness "$scratch/all.log" --sample-cases=all; then
  cat "$scratch/all.log" >&2
  printf 'run-tests-progress-selftest: all-case fixture failed\n' >&2
  exit 1
fi
if ! grep -Eq 'progress: [0-9]+/4 units done,' "$scratch/all.log"; then
  cat "$scratch/all.log" >&2
  printf 'run-tests-progress-selftest: all-case heartbeat did not retain its full denominator\n' >&2
  exit 1
fi

if ! KIO_TEST_FAST_CACHE=1 \
  KIO_TEST_TYPED_ROOT_MARKER="$scratch/harness-typed-root" \
  KIO_TEST_RUN_SH_TYPED_ROOT_MARKER="$scratch/run-sh-typed-roots" \
  run_harness "$scratch/typed-root.log" --sample-cases=all; then
  cat "$scratch/typed-root.log" >&2
  printf 'run-tests-progress-selftest: run-shared typed-cache root fixture failed\n' >&2
  exit 1
fi
LC_ALL=C sort -u "$scratch/harness-typed-root" >"$scratch/harness-typed-roots-unique"
typed_root_count=$(wc -l <"$scratch/harness-typed-roots-unique" | tr -d ' ')
if [ "$typed_root_count" -ne 1 ]; then
  cat "$scratch/typed-root.log" >&2
  printf 'run-tests-progress-selftest: units observed %s typed-cache roots\n' \
    "$typed_root_count" >&2
  exit 1
fi
typed_root=$(cat "$scratch/harness-typed-roots-unique")
case "$typed_root" in
  "$scratch/tmp"/*/shared-typed-cache) ;;
  *)
    cat "$scratch/typed-root.log" >&2
    printf 'run-tests-progress-selftest: typed-cache root is not run-scoped: %s\n' \
      "$typed_root" >&2
    exit 1
    ;;
esac
if [ -e "$typed_root" ]; then
  cat "$scratch/typed-root.log" >&2
  printf 'run-tests-progress-selftest: run-scoped typed-cache root survived cleanup: %s\n' \
    "$typed_root" >&2
  exit 1
fi

cut -f2 "$scratch/run-sh-typed-roots" | LC_ALL=C sort -u \
  >"$scratch/run-sh-typed-roots-unique"
run_sh_root_count=$(wc -l <"$scratch/run-sh-typed-roots-unique" | tr -d ' ')
if [ "$run_sh_root_count" -ne 4 ]; then
  cat "$scratch/typed-root.log" >&2
  printf 'run-tests-progress-selftest: custom cases observed %s isolated typed-cache roots\n' \
    "$run_sh_root_count" >&2
  exit 1
fi
while IFS= read -r run_sh_root; do
  case "$run_sh_root" in
    "$scratch/tmp"/*/scratch/*/impl_1/typed-cache) ;;
    *)
      printf 'run-tests-progress-selftest: custom typed-cache root is not per-unit scratch: %s\n' \
        "$run_sh_root" >&2
      exit 1
      ;;
  esac
  if [ -e "$run_sh_root" ]; then
    printf 'run-tests-progress-selftest: custom typed-cache root survived cleanup: %s\n' \
      "$run_sh_root" >&2
    exit 1
  fi
done <"$scratch/run-sh-typed-roots-unique"

# Exercise the fixed cohort authority from a synthetic repository layout. The
# production path accepts only its sibling harness-owned manifest and canonical
# goldens directory; copying this small script/infra set preserves that same
# boundary without granting the ordinary scratch corpus a test-only bypass.
cohort_repo=$scratch/custom-cohort-repo
cohort_ci=$cohort_repo/ci
cohort_cases=$cohort_repo/test-data/goldens
cohort_manifest_dir=$cohort_ci/checks/orchestrators/custom-typed-cache
cohort_manifest=$cohort_manifest_dir/exec-dyn-load-goldens.tsv
cohort_run_tests=$cohort_ci/run-tests.sh
mkdir -p "$cohort_ci/infra" "$cohort_manifest_dir" "$cohort_cases"
cp "$REPO_ROOT/ci/run-tests.sh" "$cohort_run_tests"
cp "$REPO_ROOT/ci/schedule.sh" "$cohort_ci/"
for cohort_infra in sccache.sh native-compiler-proxy.sh \
  kio-compiler-proxy.sh test-runner-identity-proxy.sh; do
  cp "$REPO_ROOT/ci/infra/$cohort_infra" "$cohort_ci/infra/$cohort_infra"
done
cp -R "$scratch/cases/bucket" "$cohort_cases/bucket"

run_cohort_harness() {
  KIO_TEST_RUN_TESTS_SH=$cohort_run_tests \
    KIO_TEST_CASES_DIR=$cohort_cases \
    run_harness "$@"
}

# The four fixture scripts have identical bytes, so listing only one/two also
# proves that copying approved content to unlisted three/four does not confer
# access.
fixture_run_oid=$(git -C "$REPO_ROOT" hash-object --no-filters \
  "$cohort_cases/bucket/one/run.sh")
printf 'bucket/one/run.sh\t%s\nbucket/two/run.sh\t%s\n' \
  "$fixture_run_oid" "$fixture_run_oid" >"$cohort_manifest"
cp "$cohort_manifest" "$scratch/custom-typed-cache-cohort.good.tsv"
if ! KIO_TEST_FAST_CACHE=1 \
  KIO_TEST_RUN_SH_TYPED_ROOT_MARKER="$scratch/cohort-typed-roots" \
  run_cohort_harness "$scratch/cohort.log" --sample-cases=all \
    --custom-typed-cache-cohort=exec-dyn-load-goldens-v1; then
  cat "$scratch/cohort.log" >&2
  printf 'run-tests-progress-selftest: authenticated custom-cache cohort fixture failed\n' >&2
  exit 1
fi
cohort_root_one=$(awk -F '\t' -v p="$cohort_cases/bucket/one" \
  '$1 == p { print $2 }' "$scratch/cohort-typed-roots")
cohort_root_two=$(awk -F '\t' -v p="$cohort_cases/bucket/two" \
  '$1 == p { print $2 }' "$scratch/cohort-typed-roots")
cohort_root_three=$(awk -F '\t' -v p="$cohort_cases/bucket/three" \
  '$1 == p { print $2 }' "$scratch/cohort-typed-roots")
cohort_root_four=$(awk -F '\t' -v p="$cohort_cases/bucket/four" \
  '$1 == p { print $2 }' "$scratch/cohort-typed-roots")
if [ -z "$cohort_root_one" ] || [ "$cohort_root_one" != "$cohort_root_two" ] ||
   [ "$cohort_root_one" = "$cohort_root_three" ] ||
   [ "$cohort_root_one" = "$cohort_root_four" ] ||
   [ "$cohort_root_three" = "$cohort_root_four" ]; then
  cat "$scratch/cohort.log" >&2
  printf 'run-tests-progress-selftest: authenticated and isolated custom roots were not partitioned exactly\n' >&2
  exit 1
fi
case "$cohort_root_one" in
  "$scratch/tmp"/*/custom-typed-cache) ;;
  *)
    printf 'run-tests-progress-selftest: cohort root is not separately run-scoped: %s\n' \
      "$cohort_root_one" >&2
    exit 1
    ;;
esac
for isolated_root in "$cohort_root_three" "$cohort_root_four"; do
  case "$isolated_root" in
    "$scratch/tmp"/*/scratch/*/impl_1/typed-cache) ;;
    *)
      printf 'run-tests-progress-selftest: unlisted custom root is not isolated: %s\n' \
        "$isolated_root" >&2
      exit 1
      ;;
  esac
done
for cleaned_root in "$cohort_root_one" "$cohort_root_three" "$cohort_root_four"; do
  if [ -e "$cleaned_root" ]; then
    printf 'run-tests-progress-selftest: custom typed-cache root survived cleanup: %s\n' \
      "$cleaned_root" >&2
    exit 1
  fi
done

# Source mutation after preflight cannot change the code that runs: one alters
# two's source file in a serial run, while two still executes its already-
# authenticated master snapshot with the original case cwd.
cp "$cohort_cases/bucket/two/run.sh" "$scratch/two.run.sh.original"
if ! KIO_TEST_JOBS=1 KIO_TEST_FAST_CACHE=1 \
  KIO_TEST_RUN_SH_TYPED_ROOT_MARKER="$scratch/cohort-source-mutation-roots" \
  KIO_TEST_MUTATE_FROM_CASE="$cohort_cases/bucket/one" \
  KIO_TEST_MUTATE_RUN_SH="$cohort_cases/bucket/two/run.sh" \
  run_cohort_harness "$scratch/cohort-source-mutation.log" \
    --custom-typed-cache-cohort=exec-dyn-load-goldens-v1 \
    '^bucket/one$' '^bucket/two$'; then
  cat "$scratch/cohort-source-mutation.log" >&2
  printf 'run-tests-progress-selftest: source mutation escaped the authenticated snapshot\n' >&2
  exit 1
fi
source_mutation_rows=$(wc -l <"$scratch/cohort-source-mutation-roots" | tr -d ' ')
if [ "$source_mutation_rows" -ne 2 ] ||
   ! grep -q '^exit 97$' "$cohort_cases/bucket/two/run.sh"; then
  cat "$scratch/cohort-source-mutation.log" >&2
  printf 'run-tests-progress-selftest: source-mutation fixture did not exercise both snapshots\n' >&2
  exit 1
fi
cp "$scratch/two.run.sh.original" "$cohort_cases/bucket/two/run.sh"
chmod +x "$cohort_cases/bucket/two/run.sh"

# A mutation of the authenticated master itself is caught by the per-impl
# launch copy/hash check before the affected script executes.
set +e
  KIO_TEST_JOBS=1 KIO_TEST_FAST_CACHE=1 \
  KIO_TEST_RUN_SH_TYPED_ROOT_MARKER="$scratch/cohort-master-mutation-roots" \
  KIO_TEST_MUTATE_FROM_CASE="$cohort_cases/bucket/one" \
  KIO_TEST_MUTATE_AUTHENTICATED_SNAPSHOT='bucket/two/run.sh' \
  run_cohort_harness "$scratch/cohort-master-mutation.log" \
    --custom-typed-cache-cohort=exec-dyn-load-goldens-v1 \
    '^bucket/one$' '^bucket/two$'
master_mutation_status=$?
set -e
master_mutation_rows=$(wc -l <"$scratch/cohort-master-mutation-roots" | tr -d ' ')
if [ "$master_mutation_status" -ne 1 ] || [ "$master_mutation_rows" -ne 1 ] ||
   ! grep -q 'authenticated custom run.sh snapshot changed before execution' \
     "$scratch/cohort-master-mutation.log"; then
  cat "$scratch/cohort-master-mutation.log" >&2
  printf 'run-tests-progress-selftest: authenticated-master mutation was not rejected at launch\n' >&2
  exit 1
fi

assert_bad_custom_cohort() {
  abcc_label=$1
  abcc_pattern=$2
  shift 2
  rm -f "$scratch/bad-cohort-ran"
  set +e
  KIO_TEST_FAST_CACHE=1 \
    KIO_TEST_RUN_SH_TYPED_ROOT_MARKER="$scratch/bad-cohort-ran" \
    run_cohort_harness "$scratch/bad-cohort-$abcc_label.log" "$@"
  abcc_status=$?
  set -e
  if [ "$abcc_status" -ne 2 ] || [ -e "$scratch/bad-cohort-ran" ] ||
     ! grep -q "$abcc_pattern" "$scratch/bad-cohort-$abcc_label.log"; then
    cat "$scratch/bad-cohort-$abcc_label.log" >&2
    printf 'run-tests-progress-selftest: invalid custom cohort accepted: %s\n' \
      "$abcc_label" >&2
    exit 1
  fi
}

printf 'bucket/one/run.sh\t0000000000000000000000000000000000000000\n' \
  >"$cohort_manifest"
assert_bad_custom_cohort stale 'script identity changed' \
  --custom-typed-cache-cohort=exec-dyn-load-goldens-v1

printf 'bucket/one/run.sh\t%s\nbucket/one/run.sh\t%s\n' \
  "$fixture_run_oid" "$fixture_run_oid" >"$cohort_manifest"
assert_bad_custom_cohort duplicate 'must contain non-empty, normalized' \
  --custom-typed-cache-cohort=exec-dyn-load-goldens-v1

printf '../bucket/one/run.sh\t%s\n' "$fixture_run_oid" >"$cohort_manifest"
assert_bad_custom_cohort unsafe 'must contain non-empty, normalized' \
  --custom-typed-cache-cohort=exec-dyn-load-goldens-v1

printf 'bucket/one/run.sh\tnot-an-object-id\n' >"$cohort_manifest"
assert_bad_custom_cohort malformed-hash 'must contain non-empty, normalized' \
  --custom-typed-cache-cohort=exec-dyn-load-goldens-v1

rm "$cohort_manifest"
ln -s "$scratch/custom-typed-cache-cohort.good.tsv" "$cohort_manifest"
assert_bad_custom_cohort symlink 'manifest must be a regular non-symlink file' \
  --custom-typed-cache-cohort=exec-dyn-load-goldens-v1

rm "$cohort_manifest"
cp "$scratch/custom-typed-cache-cohort.good.tsv" "$cohort_manifest"
assert_bad_custom_cohort duplicate-option 'may be specified only once' \
  --custom-typed-cache-cohort=exec-dyn-load-goldens-v1 \
  --custom-typed-cache-cohort=exec-dyn-load-goldens-v1

assert_bad_custom_cohort unknown-id 'unknown --custom-typed-cache-cohort' \
  --custom-typed-cache-cohort=no-such-cohort

# The same fixed ID cannot be applied to a non-golden cases root, which closes
# passthrough attempts from contrib/POC/castle orchestrators.
rm -f "$scratch/noncanonical-cohort-ran"
set +e
KIO_TEST_FAST_CACHE=1 \
  KIO_TEST_RUN_SH_TYPED_ROOT_MARKER="$scratch/noncanonical-cohort-ran" \
  run_harness "$scratch/noncanonical-cohort.log" \
    --custom-typed-cache-cohort=exec-dyn-load-goldens-v1
noncanonical_status=$?
set -e
if [ "$noncanonical_status" -ne 2 ] ||
   [ -e "$scratch/noncanonical-cohort-ran" ] ||
   ! grep -q 'valid only for the canonical goldens directory' \
     "$scratch/noncanonical-cohort.log"; then
  cat "$scratch/noncanonical-cohort.log" >&2
  printf 'run-tests-progress-selftest: fixed custom cohort escaped canonical goldens\n' >&2
  exit 1
fi

for mode in ordinary show-output update-expected; do
  set -- --sample-cases=1 --case-seed=fixed
  case "$mode" in
    ordinary) ;;
    show-output) set -- "$@" --show-output ;;
    update-expected) set -- "$@" --update-expected ;;
  esac
  requested_jobs=4
  [ "$mode" != ordinary ] || requested_jobs=1
  if ! KIO_TEST_JOBS=$requested_jobs KIO_TEST_EXPECT_JOBS=1 \
    KIO_TEST_REQUIRE_WORK=1 KIO_TEST_FAST_CACHE=1 \
    run_harness "$scratch/serial-$mode.log" "$@"; then
    cat "$scratch/serial-$mode.log" >&2
    printf 'run-tests-progress-selftest: serial %s worker bypassed scheduling or failed\n' \
      "$mode" >&2
    exit 1
  fi
done

if ! KIO_TEST_JOBS=8 KIO_TEST_EXPECT_JOBS=8 \
  KIO_TEST_REQUIRE_WORK=1 KIO_TEST_FAST_CACHE=1 \
  run_harness "$scratch/single-unit-capacity.log" '^bucket/one$'; then
  cat "$scratch/single-unit-capacity.log" >&2
  printf 'run-tests-progress-selftest: one selected unit lowered the shared capacity\n' >&2
  exit 1
fi
if grep -q '^jobs: ' "$scratch/single-unit-capacity.log" ||
   ! grep -q '^  fixture: 1 passed, 0 failed$' \
     "$scratch/single-unit-capacity.log"; then
  cat "$scratch/single-unit-capacity.log" >&2
  printf 'run-tests-progress-selftest: one selected unit did not execute once serially\n' >&2
  exit 1
fi

if ! KIO_TEST_OMIT_JOBS=1 KIO_TEST_SCHEDULE_JOBS=2 \
  KIO_TEST_EXPECT_JOBS=2 KIO_TEST_REQUIRE_WORK=1 KIO_TEST_FAST_CACHE=1 \
  run_harness "$scratch/inherited-single-unit.log" '^bucket/one$'; then
  cat "$scratch/inherited-single-unit.log" >&2
  printf 'run-tests-progress-selftest: one selected unit replaced inherited capacity\n' >&2
  exit 1
fi

if ! KIO_TEST_JOBS=2 KIO_TEST_EXPECT_JOBS=2 KIO_TEST_REQUIRE_WORK=1 \
  KIO_TEST_FAST_CACHE=1 run_harness "$scratch/two-jobs.log" --sample-cases=all; then
  cat "$scratch/two-jobs.log" >&2
  printf 'run-tests-progress-selftest: resolved parallel capacity was not published\n' >&2
  exit 1
fi

if ! KIO_TEST_OMIT_JOBS=1 KIO_TEST_SCHEDULE_JOBS=2 \
  KIO_TEST_EXPECT_JOBS=2 KIO_TEST_REQUIRE_WORK=1 KIO_TEST_FAST_CACHE=1 \
  run_harness "$scratch/inherited-jobs.log" --sample-cases=all; then
  cat "$scratch/inherited-jobs.log" >&2
  printf 'run-tests-progress-selftest: omitted local --jobs replaced the inherited capacity\n' >&2
  exit 1
fi

if ! KIO_TEST_OMIT_JOBS=1 KIO_TEST_SCHEDULE_JOBS=2 \
  KIO_TEST_EXPECT_JOBS=2 KIO_TEST_REQUIRE_WORK=1 KIO_TEST_FAST_CACHE=1 \
  run_harness "$scratch/inherited-serial.log" --sample-cases=1 \
    --case-seed=fixed --show-output; then
  cat "$scratch/inherited-serial.log" >&2
  printf 'run-tests-progress-selftest: serial mode replaced the inherited capacity\n' >&2
  exit 1
fi

if ! KIO_CI_SCHEDULE=DISABLE KIO_TEST_JOBS=1 KIO_TEST_FAST_CACHE=1 \
  run_harness "$scratch/disabled.log" --sample-cases=1 --case-seed=fixed; then
  cat "$scratch/disabled.log" >&2
  printf 'run-tests-progress-selftest: disabled scheduler worker recursed or failed\n' >&2
  exit 1
fi

printf 'run-tests-progress-selftest: ok\n'
