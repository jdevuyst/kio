#!/bin/sh
#
# Exercise the registry-backed case-coverage policy without launching builds or
# corpus workloads. POSIX sh only.
# shellcheck disable=SC2030,SC2031 # mutation fixtures intentionally shadow registry paths in subshells

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
GOLDEN_TESTS_SH=${GOLDEN_TESTS_SH:-$REPO_ROOT/ci/checks/orchestrators/golden-tests.sh}
COVERAGE_POLICY_DIR=$REPO_ROOT/ci/checks/orchestrators/lib
export COVERAGE_POLICY_DIR

# The implementation supplies pure policy helpers through this shared file.
# Keeping this first assertion at the source boundary makes the test causal:
# it is red on the pre-policy harness before any fixture can pass vacuously.
if [ ! -f "$COVERAGE_POLICY_DIR/coverage-policy.sh" ]; then
  printf 'coverage-policy-selftest: missing shared coverage policy\n' >&2
  exit 1
fi

# shellcheck source=/dev/null
. "$COVERAGE_POLICY_DIR/coverage-policy.sh"

coverage_policy_validate_registry "$REPO_ROOT/ci/checks/orchestrators"

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/coverage-policy-selftest.XXXXXX") || exit 2
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

assert_bad_registry() {
  abr_label=$1 abr_registry=$2
  if (
    COVERAGE_ORCHESTRATOR_REGISTRY=$abr_registry
    export COVERAGE_ORCHESTRATOR_REGISTRY
    coverage_policy_validate_registry "$REPO_ROOT/ci/checks/orchestrators"
  ) >/dev/null 2>&1; then
    printf 'coverage-policy-selftest: malformed registry accepted: %s\n' "$abr_label" >&2
    exit 1
  fi
}

awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $3="PARTIAL" } { print }' \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" >"$scratch/noncomplete.tsv"
assert_bad_registry noncomplete "$scratch/noncomplete.tsv"

awk -F '\t' 'BEGIN { OFS="\t" } { print } NR == 2 { print }' \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" >"$scratch/duplicate-owner.tsv"
assert_bad_registry duplicate-owner "$scratch/duplicate-owner.tsv"

sed '/^website-e2e[.]sh/d' "$COVERAGE_ORCHESTRATOR_REGISTRY" \
  >"$scratch/missing-owner.tsv"
assert_bad_registry missing-owner "$scratch/missing-owner.tsv"

assert_eq() {
  ae_want=$1 ae_got=$2 ae_label=$3
  if [ "$ae_want" != "$ae_got" ]; then
    printf 'coverage-policy-selftest: %s: wanted <%s>, got <%s>\n' \
      "$ae_label" "$ae_want" "$ae_got" >&2
    exit 1
  fi
}

coverage_policy_validate_policy all
coverage_policy_validate_policy sample
coverage_policy_validate_policy 7
coverage_policy_validate_policy smoke

for rejected in 0 00 000 -1 ALL '../smoke' 'smoke/name' ''; do
  if coverage_policy_validate_policy "$rejected" >/dev/null 2>&1; then
    printf 'coverage-policy-selftest: accepted invalid policy <%s>\n' "$rejected" >&2
    exit 1
  fi
done

assert_eq dyn-load-prime \
  "$(coverage_policy_verification_scope_for_impl dyn-load-prime@kio-prime)" \
  'fixed implementation lookup'
assert_eq golden-tests.sh \
  "$(coverage_policy_owner_for_scope dyn-load-prime)" 'leaf owner lookup'
assert_eq goldens "$(coverage_policy_parent_for_scope dyn-load-prime)" \
  'leaf parent lookup'
assert_eq "$REPO_ROOT/ci/checks/orchestrators/case-coverage/dyn-load-prime/smoke.cases" \
  "$(coverage_policy_named_set_file dyn-load-prime smoke)" \
  'named-set lookup'

assert_eq root-corpus "$(coverage_policy_classification_for_scope goldens)" \
  'root scope classification'
assert_eq fixed-verification \
  "$(coverage_policy_classification_for_scope dyn-load-prime)" \
  'verification scope classification'

owners_dir=$scratch/owners
mkdir "$owners_dir"
awk -F '\t' 'NR > 1 { print $1 }' "$COVERAGE_ORCHESTRATOR_REGISTRY" \
  >"$scratch/owner-list"
while IFS= read -r owner; do
  : >"$owners_dir/$owner"
done <"$scratch/owner-list"

assert_registry_failure() {
  arf_label=$1
  arf_owners=$2
  arf_orchestrators=$3
  arf_verifications=$4
  arf_named=$5
  if (
    COVERAGE_ORCHESTRATOR_REGISTRY=$arf_orchestrators
    COVERAGE_VERIFICATION_SCOPES=$arf_verifications
    COVERAGE_NAMED_SETS=$arf_named
    export COVERAGE_ORCHESTRATOR_REGISTRY COVERAGE_VERIFICATION_SCOPES \
      COVERAGE_NAMED_SETS
    coverage_policy_validate_registry "$arf_owners"
  ) >/dev/null 2>&1; then
    printf 'coverage-policy-selftest: malformed policy registry accepted: %s\n' \
      "$arf_label" >&2
    exit 1
  fi
}

: >"$owners_dir/zz-unregistered.sh"
assert_registry_failure unregistered-owner "$owners_dir" \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" "$COVERAGE_VERIFICATION_SCOPES" \
  "$COVERAGE_NAMED_SETS"
rm "$owners_dir/zz-unregistered.sh"

: >"$owners_dir/.hidden-unregistered.sh"
assert_registry_failure hidden-unregistered-owner "$owners_dir" \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" "$COVERAGE_VERIFICATION_SCOPES" \
  "$COVERAGE_NAMED_SETS"
rm "$owners_dir/.hidden-unregistered.sh"

: >"$owners_dir/..sh"
assert_registry_failure double-dot-unregistered-owner "$owners_dir" \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" "$COVERAGE_VERIFICATION_SCOPES" \
  "$COVERAGE_NAMED_SETS"
rm "$owners_dir/..sh"

: >"$owners_dir/.sh"
assert_registry_failure shortest-hidden-unregistered-owner "$owners_dir" \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" "$COVERAGE_VERIFICATION_SCOPES" \
  "$COVERAGE_NAMED_SETS"
rm "$owners_dir/.sh"

ln -s "$owners_dir/golden-tests.sh" "$owners_dir/.unlaunched-symlink.sh"
coverage_policy_validate_registry "$owners_dir"
rm "$owners_dir/.unlaunched-symlink.sh"

cp "$COVERAGE_ORCHESTRATOR_REGISTRY" "$scratch/null-root.tsv"
printf 'zz-null.sh\t-\tCOMPLETE\n' >>"$scratch/null-root.tsv"
: >"$owners_dir/zz-null.sh"
(
  COVERAGE_ORCHESTRATOR_REGISTRY=$scratch/null-root.tsv
  export COVERAGE_ORCHESTRATOR_REGISTRY
  coverage_policy_validate_registry "$owners_dir"
) >/dev/null
rm "$owners_dir/zz-null.sh"

cp "$COVERAGE_ORCHESTRATOR_REGISTRY" "$scratch/stale-owner.tsv"
printf 'zz-stale.sh\t-\tCOMPLETE\n' >>"$scratch/stale-owner.tsv"
assert_registry_failure stale-owner "$owners_dir" "$scratch/stale-owner.tsv" \
  "$COVERAGE_VERIFICATION_SCOPES" "$COVERAGE_NAMED_SETS"

sed '1s/orchestrator/owner/' "$COVERAGE_ORCHESTRATOR_REGISTRY" \
  >"$scratch/bad-owner-header.tsv"
assert_registry_failure owner-header "$owners_dir" "$scratch/bad-owner-header.tsv" \
  "$COVERAGE_VERIFICATION_SCOPES" "$COVERAGE_NAMED_SETS"

sed '1s/classification/class/' "$COVERAGE_VERIFICATION_SCOPES" \
  >"$scratch/bad-verification-header.tsv"
assert_registry_failure verification-header "$owners_dir" \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" "$scratch/bad-verification-header.tsv" \
  "$COVERAGE_NAMED_SETS"

for verification_mutation in wrong-owner root-collision wrong-class duplicate-impl; do
  case $verification_mutation in
    wrong-owner)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $2="website-e2e.sh" } { print }' \
        "$COVERAGE_VERIFICATION_SCOPES" ;;
    root-collision)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $1="goldens" } { print }' \
        "$COVERAGE_VERIFICATION_SCOPES" ;;
    wrong-class)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $4="optional" } { print }' \
        "$COVERAGE_VERIFICATION_SCOPES" ;;
    duplicate-impl)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 3 { $5="kio-prime@js" } { print }' \
        "$COVERAGE_VERIFICATION_SCOPES" ;;
  esac >"$scratch/verification-$verification_mutation.tsv"
  assert_registry_failure "$verification_mutation" "$owners_dir" \
    "$COVERAGE_ORCHESTRATOR_REGISTRY" \
    "$scratch/verification-$verification_mutation.tsv" "$COVERAGE_NAMED_SETS"
done

printf '%s\n' kio-prime@js dyn-load-prime@kio-prime \
  >"$scratch/defined-fixed-impls"
coverage_policy_validate_owner_fixed_impls golden-tests.sh \
  "$scratch/defined-fixed-impls"
awk -F '\t' 'BEGIN { OFS="\t" } NR == 3 { $5="missing@fixed" } { print }' \
  "$COVERAGE_VERIFICATION_SCOPES" >"$scratch/missing-fixed.tsv"
if (
  COVERAGE_VERIFICATION_SCOPES=$scratch/missing-fixed.tsv
  export COVERAGE_VERIFICATION_SCOPES
  coverage_policy_validate_owner_fixed_impls golden-tests.sh \
    "$scratch/defined-fixed-impls"
) >/dev/null 2>&1; then
  printf 'coverage-policy-selftest: owner accepted an undefined fixed implementation\n' >&2
  exit 1
fi

sed '1s/manifest/path/' "$COVERAGE_NAMED_SETS" \
  >"$scratch/bad-named-header.tsv"
assert_registry_failure named-header "$owners_dir" \
  "$COVERAGE_ORCHESTRATOR_REGISTRY" "$COVERAGE_VERIFICATION_SCOPES" \
  "$scratch/bad-named-header.tsv"
for named_mutation in unknown-scope bad-path bad-class zero-min allow-overlap; do
  case $named_mutation in
    unknown-scope)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $1="unknown" } { print }' \
        "$COVERAGE_NAMED_SETS" ;;
    bad-path)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $3="ci/../smoke.cases" } { print }' \
        "$COVERAGE_NAMED_SETS" ;;
    bad-class)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $4="smoke" } { print }' \
        "$COVERAGE_NAMED_SETS" ;;
    zero-min)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $5="0" } { print }' \
        "$COVERAGE_NAMED_SETS" ;;
    allow-overlap)
      awk -F '\t' 'BEGIN { OFS="\t" } NR == 2 { $6="allow" } { print }' \
        "$COVERAGE_NAMED_SETS" ;;
  esac >"$scratch/named-$named_mutation.tsv"
  assert_registry_failure "$named_mutation" "$owners_dir" \
    "$COVERAGE_ORCHESTRATOR_REGISTRY" "$COVERAGE_VERIFICATION_SCOPES" \
    "$scratch/named-$named_mutation.tsv"
done

registry_tmp=$scratch/registry-tmp
mkdir "$registry_tmp"
(
  TMPDIR=$registry_tmp
  export TMPDIR
  coverage_policy_validate_registry "$REPO_ROOT/ci/checks/orchestrators"
)
if directory_has_entry "$registry_tmp"; then
  printf 'coverage-policy-selftest: registry validation leaked temporary files\n' >&2
  exit 1
fi
(
  unset TMPDIR
  cd /
  coverage_policy_validate_registry "$REPO_ROOT/ci/checks/orchestrators"
)

named_repo=$scratch/named-repo
named_lib=$named_repo/ci/checks/orchestrators/lib
named_manifest=$named_repo/ci/checks/orchestrators/case-coverage/check/smoke.cases
named_cases=$named_repo/cases
mkdir -p "${named_manifest%/*}" "$named_cases/bucket/success" \
  "$named_cases/bucket/failure" "$named_cases/bucket/known" \
  "$named_cases/bucket/outside"
cat >"$named_lib.tmp" <<'EOF'
scope	policy	manifest	classification	minimum_non_known_failing_success	known_failing_overlap
check	smoke	ci/checks/orchestrators/case-coverage/check/smoke.cases	success-smoke	1	reject
EOF
mkdir -p "$named_lib"
mv "$named_lib.tmp" "$named_lib/named-sets.tsv"
printf '0\n' >"$named_cases/bucket/success/expected.exit"
printf '1\n' >"$named_cases/bucket/failure/expected.exit"
printf '1\n' >"$named_cases/bucket/known/expected.exit"
printf '0\n' >"$named_cases/bucket/outside/expected.exit"
: >"$named_cases/bucket/known/KNOWN_FAILING"
printf '%s\n' bucket/failure bucket/known bucket/success \
  >"$named_repo/canonical.cases"
printf 'bucket/success\n' >"$named_manifest"
(
  COVERAGE_POLICY_DIR=$named_lib
  COVERAGE_NAMED_SETS=$named_lib/named-sets.tsv
  export COVERAGE_POLICY_DIR COVERAGE_NAMED_SETS
  coverage_policy_validate_named_set check smoke \
    "$named_repo/canonical.cases" "$named_cases"
)

assert_bad_named_set() {
  abns_label=$1
  if (
    COVERAGE_POLICY_DIR=$named_lib
    COVERAGE_NAMED_SETS=$named_lib/named-sets.tsv
    export COVERAGE_POLICY_DIR COVERAGE_NAMED_SETS
    coverage_policy_validate_named_set check smoke \
      "$named_repo/canonical.cases" "$named_cases"
  ) >/dev/null 2>&1; then
    printf 'coverage-policy-selftest: invalid named set accepted: %s\n' \
      "$abns_label" >&2
    exit 1
  fi
}

printf 'bucket/failure\n' >"$named_manifest"
assert_bad_named_set no-success
printf 'bucket/known\n' >"$named_manifest"
assert_bad_named_set known-failing-overlap
printf 'bucket/outside\n' >"$named_manifest"
assert_bad_named_set outside-canonical
printf 'bucket/success\n' >"$named_manifest"
: >"$named_repo/canonical.cases"
assert_bad_named_set empty-canonical
printf '%s\n' bucket/failure bucket/known bucket/success \
  >"$named_repo/canonical.cases"
: >"$named_manifest"
assert_bad_named_set empty-manifest
printf '%s\n' bucket/success bucket/success >"$named_manifest"
assert_bad_named_set duplicate-manifest
rm "$named_manifest"
ln -s "$named_repo/canonical.cases" "$named_manifest"
assert_bad_named_set symlink-manifest
rm "$named_manifest"
mkdir -p "$named_cases/10" "$named_cases/2"
printf '0\n' >"$named_cases/10/expected.exit"
printf '0\n' >"$named_cases/2/expected.exit"
printf '%s\n' 10 2 >"$named_repo/canonical.cases"
printf '%s\n' 10 2 >"$named_manifest"
(
  COVERAGE_POLICY_DIR=$named_lib
  COVERAGE_NAMED_SETS=$named_lib/named-sets.tsv
  export COVERAGE_POLICY_DIR COVERAGE_NAMED_SETS
  coverage_policy_validate_named_set check smoke \
    "$named_repo/canonical.cases" "$named_cases"
)
printf '%s\n' 2 10 >"$named_manifest"
assert_bad_named_set numeric-unsorted-manifest

# Drive the public golden owner itself with fake build/run boundaries. This
# proves direct implementation selection and scoped-policy precedence without
# launching Cargo, a compiler, or a corpus worker.
direct_repo=$scratch/direct-repo
direct_orchestrators=$direct_repo/ci/checks/orchestrators
direct_lib=$direct_orchestrators/lib
direct_manifest=$direct_orchestrators/case-coverage/dyn-load-prime/smoke.cases
direct_cases=$direct_repo/test-data/goldens
direct_run_log=$scratch/direct-run.log
direct_build_log=$scratch/direct-build.log
direct_output=$scratch/direct-output.log
mkdir -p "$direct_lib" "${direct_manifest%/*}" \
  "$direct_repo/ci/infra" \
  "$direct_repo/ci/infra/kio-prime-check-rs" \
  "$direct_repo/ci/infra/kio-test-runner-rs/dyn-load-prime-driver" \
  "$direct_repo/kio-rs" "$direct_repo/test-data/poc/dyn_load_prime/workdir" \
  "$direct_cases/00_success/regular" \
  "$direct_cases/00_success/prime" \
  "$direct_cases/00_success/dyn_other" \
  "$direct_cases/00_success/dyn_smoke" \
  "$scratch/direct-tmp"
cp "$GOLDEN_TESTS_SH" "$direct_orchestrators/golden-tests.sh"
cp "$REPO_ROOT/ci/checks/orchestrators/lib/common.sh" "$direct_lib/common.sh"
cp "$REPO_ROOT/ci/checks/orchestrators/lib/coverage-policy.sh" \
  "$direct_lib/coverage-policy.sh"
cat >"$direct_lib/orchestrator-registry.tsv" <<'EOF'
orchestrator	root_scope	declaration
golden-tests.sh	goldens	COMPLETE
EOF
cat >"$direct_lib/verification-scopes.tsv" <<'EOF'
scope	owner	parent	classification	fixed_impl	toolchain_class
kio-prime	golden-tests.sh	goldens	fixed-verification	kio-prime@js	core
dyn-load-prime	golden-tests.sh	goldens	fixed-verification	dyn-load-prime@kio-prime	core
EOF
cat >"$direct_lib/named-sets.tsv" <<'EOF'
scope	policy	manifest	classification	minimum_non_known_failing_success	known_failing_overlap
dyn-load-prime	smoke	ci/checks/orchestrators/case-coverage/dyn-load-prime/smoke.cases	success-smoke	1	reject
EOF
printf '00_success/dyn_smoke\n' >"$direct_manifest"
for direct_case in regular prime dyn_other dyn_smoke; do
  printf '0\n' >"$direct_cases/00_success/$direct_case/expected.exit"
done
: >"$direct_cases/00_success/prime/IS_KIO_PRIME"
: >"$direct_cases/00_success/dyn_other/DYN_LOAD_PRIME"
: >"$direct_cases/00_success/dyn_smoke/DYN_LOAD_PRIME"

cat >"$direct_repo/ci/infra/sccache.sh" <<'EOF'
kio_configure_sccache_environment() {
  :
}
kio_ensure_sccache_ready() {
  :
}
EOF
cat >"$direct_repo/ci/schedule.sh" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}" in
  --prepare)
    [ "$#" -eq 1 ] || exit 97
    printf '%s\n' "$KIO_TEST_FAKE_SCHEDULER"
    ;;
  --readiness)
    [ "${2:-}" = -- ] && [ "$#" -gt 2 ] || exit 97
    ;;
  --resource)
    [ "${2:-}" = cargo ] && [ "${3:-}" = -- ] && [ "$#" -gt 3 ] || exit 97
    shift 3
    exec "$@"
    ;;
  *) exit 97 ;;
esac
EOF
cat >"$direct_repo/ci/cargo.sh" <<'EOF'
#!/bin/sh
set -eu
{
  printf 'cargo'
  for cargo_arg in "$@"; do printf ' %s' "$cargo_arg"; done
  printf '\n'
} >>"$KIO_TEST_DIRECT_BUILD_LOG"
target_dir=target
binary=
all_bins=0
while [ $# -gt 0 ]; do
  case $1 in
    --target-dir)
      shift
      [ $# -gt 0 ] || exit 2
      target_dir=$1
      ;;
    --target-dir=*) target_dir=${1#--target-dir=} ;;
    --bin)
      shift
      [ $# -gt 0 ] || exit 2
      binary=$1
      ;;
    --bin=*) binary=${1#--bin=} ;;
    --bins) all_bins=1 ;;
  esac
  shift
done
if [ -n "$target_dir" ]; then
  mkdir -p "$target_dir/debug"
  case $binary:$PWD in
    kio-prime:*) : >"$target_dir/debug/kio-prime" ;;
    :*/kio-prime-check-rs) : >"$target_dir/debug/kio-prime-check" ;;
    :*/kio-rs)
      printf '#!/bin/sh\nprintf "snapshot-cli\\n"\n' >"$target_dir/debug/kio"
      chmod +x "$target_dir/debug/kio"
      [ "$all_bins" = 0 ] || : >"$target_dir/debug/kio-prime"
      ;;
  esac
fi
EOF
cat >"$direct_repo/ci/infra/kio-test-runner-rs/dyn-load-prime-driver/build-driver.sh" <<'EOF'
#!/bin/sh
set -eu
printf 'driver\n' >>"$KIO_TEST_DIRECT_BUILD_LOG"
[ "$1" = "$ORCHESTRATOR_TMP/kio" ] || {
  printf 'dynamic driver did not receive the private full compiler\n' >&2
  exit 91
}
[ "$("$1")" = snapshot-cli ]
publication=$3
mkdir -p "$publication"
: >"$publication/driver.js"
printf '%s/driver.js\n' "$publication"
EOF
cat >"$direct_repo/ci/run-tests.sh" <<'EOF'
#!/bin/sh
set -eu
mode=regular
for run_arg in "$@"; do
  case $run_arg in
    --prime-only) mode=prime ;;
    --dyn-load-prime-only) mode=dyn-load-prime ;;
  esac
done
tab=$(printf '\t')
printf 'CALL%s%s\n' "$tab" "$mode" >>"$KIO_TEST_DIRECT_RUN_LOG"
for run_arg in "$@"; do
  printf 'ARG%s%s%s%s\n' "$tab" "$mode" "$tab" "$run_arg" \
    >>"$KIO_TEST_DIRECT_RUN_LOG"
  case $run_arg in
    --impl-def=*)
      compiler=${run_arg#*,kio=}
      compiler=${compiler%%,*}
      case $run_arg in
        --impl-def=name=kio-prime@*) continue ;;
      esac
      [ "$compiler" = "$ORCHESTRATOR_TMP/kio" ] || {
        printf 'corpus did not receive the private full compiler\n' >&2
        exit 91
      }
      [ "$("$compiler")" = snapshot-cli ]
      mkdir -p kio-rs/target/debug
      printf '#!/bin/sh\nexit 92\n' >kio-rs/target/debug/kio
      printf '#!/bin/sh\nexit 93\n' >kio-rs/target/kio-corpus-tools/kio-lsp-cli/debug/kio
      [ "$("$compiler")" = snapshot-cli ]
      ;;
  esac
done
EOF
chmod +x "$direct_repo/ci/schedule.sh" "$direct_repo/ci/cargo.sh" \
  "$direct_repo/ci/run-tests.sh" \
  "$direct_repo/ci/infra/kio-test-runner-rs/dyn-load-prime-driver/build-driver.sh"

run_direct_golden() {
  rdg_label=$1
  rdg_expected=$2
  shift 2
  : >"$direct_run_log"
  : >"$direct_build_log"
  set +e
  TMPDIR=$scratch/direct-tmp \
    KIO_TEST_FAKE_SCHEDULER=$direct_repo/ci/fake-scheduler \
    KIO_TEST_DIRECT_RUN_LOG=$direct_run_log \
    KIO_TEST_DIRECT_BUILD_LOG=$direct_build_log \
    sh "$direct_orchestrators/golden-tests.sh" "$@" \
      --jobs=1 --compiler-jobs=1 >"$direct_output" 2>&1
  rdg_status=$?
  set -e
  if [ "$rdg_status" -ne "$rdg_expected" ]; then
    cat "$direct_output" >&2
    printf 'coverage-policy-selftest: direct golden %s exited %s, expected %s\n' \
      "$rdg_label" "$rdg_status" "$rdg_expected" >&2
    exit 1
  fi
}

direct_call_count() {
  awk -F '\t' -v mode="$1" \
    '$1 == "CALL" && $2 == mode { count++ } END { print count + 0 }' \
    "$direct_run_log"
}

direct_arg_count() {
  dac_mode=$1 dac_arg=$2
  awk -F '\t' -v mode="$dac_mode" -v arg="$dac_arg" \
    '$1 == "ARG" && $2 == mode && $3 == arg { count++ }
     END { print count + 0 }' "$direct_run_log"
}

direct_arg_prefix_count() {
  dapc_mode=$1 dapc_prefix=$2
  awk -F '\t' -v mode="$dapc_mode" -v prefix="$dapc_prefix" \
    '$1 == "ARG" && $2 == mode && index($3, prefix) == 1 { count++ }
     END { print count + 0 }' "$direct_run_log"
}

assert_direct_calls() {
  adc_regular=$1 adc_prime=$2 adc_dyn=$3 adc_label=$4
  assert_eq "$adc_regular" "$(direct_call_count regular)" "$adc_label regular calls"
  assert_eq "$adc_prime" "$(direct_call_count prime)" "$adc_label prime calls"
  assert_eq "$adc_dyn" "$(direct_call_count dyn-load-prime)" "$adc_label dyn calls"
}

assert_direct_impl() {
  adi_mode=$1 adi_impl=$2 adi_count=$3 adi_label=$4
  assert_eq "$adi_count" \
    "$(direct_arg_prefix_count "$adi_mode" "--impl-def=name=$adi_impl,")" \
    "$adi_label $adi_impl rows"
}

assert_direct_impl_totals() {
  adit_regular=$1 adit_prime=$2 adit_dyn=$3 adit_label=$4
  assert_eq "$adit_regular" \
    "$(direct_arg_prefix_count regular --impl-def=)" \
    "$adit_label total regular impl rows"
  assert_eq "$adit_prime" \
    "$(direct_arg_prefix_count prime --impl-def=)" \
    "$adit_label total prime impl rows"
  assert_eq "$adit_dyn" \
    "$(direct_arg_prefix_count dyn-load-prime --impl-def=)" \
    "$adit_label total dyn impl rows"
}

assert_prebuild_rejection() {
  apr_label=$1 apr_pattern=$2
  shift 2
  run_direct_golden "$apr_label" 2 "$@"
  if ! grep -Fq -- "$apr_pattern" "$direct_output" ||
     [ -s "$direct_run_log" ] || [ -s "$direct_build_log" ]; then
    cat "$direct_output" >&2
    printf 'coverage-policy-selftest: %s was not rejected before build/dispatch\n' \
      "$apr_label" >&2
    exit 1
  fi
}

# I04: explicit fixed selection plus augmentation remains one fixed row.
run_direct_golden explicit-fixed-dedupe 0 \
  --impls=kio@js,dyn-load-prime@kio-prime \
  --impl-verification=dyn-load-prime
assert_direct_calls 1 0 1 explicit-fixed-dedupe
assert_direct_impl_totals 1 0 1 explicit-fixed-dedupe
assert_direct_impl regular kio@js 1 explicit-fixed-dedupe
assert_direct_impl dyn-load-prime dyn-load-prime@kio-prime 1 \
  explicit-fixed-dedupe

# I05/I06 and W03: SAMPLE/FULL already carry both fixed groups exactly once;
# an explicit request cannot duplicate either. Implementation selection does
# not disable the dynamic group's default case sampling.
run_direct_golden sample-fixed-dedupe 0 --impls=SAMPLE_IMPL \
  --impl-verification=dyn-load-prime
assert_direct_calls 1 1 1 sample-fixed-dedupe
assert_direct_impl_totals 8 1 1 sample-fixed-dedupe
assert_eq 1 \
  "$(direct_arg_count regular "--custom-typed-cache-cohort=exec-dyn-load-goldens-v1")" \
  'sample regular custom typed-cache cohort'
assert_eq 0 \
  "$(direct_arg_prefix_count prime --custom-typed-cache-cohort=)" \
  'sample prime custom typed-cache cohort exclusion'
assert_eq 0 \
  "$(direct_arg_prefix_count dyn-load-prime --custom-typed-cache-cohort=)" \
  'sample dyn custom typed-cache cohort exclusion'
assert_eq 1 "$(direct_arg_count regular --impls=SAMPLE_IMPL)" \
  'sample regular selector'
assert_eq 1 "$(direct_arg_count prime --impls=SAMPLE_IMPL)" \
  'sample prime selector'
assert_eq 1 "$(direct_arg_count dyn-load-prime --impls=SAMPLE_IMPL)" \
  'sample dyn selector'
assert_direct_impl prime kio-prime@js 1 sample-fixed-dedupe
assert_direct_impl dyn-load-prime dyn-load-prime@kio-prime 1 \
  sample-fixed-dedupe
assert_eq 0 "$(direct_arg_prefix_count regular --sample-cases=)" \
  'default regular whole'
assert_eq 0 "$(direct_arg_prefix_count prime --sample-cases=)" \
  'default prime whole'
assert_eq 1 "$(direct_arg_count dyn-load-prime --sample-cases=50)" \
  'default dynamic budget'
assert_eq 1 "$(direct_arg_prefix_count dyn-load-prime --case-seed=)" \
  'implicit sample obtains a seed'
assert_eq 0 "$(direct_arg_count dyn-load-prime --case-seed=)" \
  'implicit sample seed is nonempty'

for sample_policy in --sample-cases --case-coverage=goldens:sample; do
  run_direct_golden explicit-sample 0 --impls=FULL_IMPL_MATRIX \
    "$sample_policy" --case-seed=fixed
  assert_eq 1 "$(direct_arg_count regular --sample-cases=100)" 'regular sample budget'
  assert_eq 1 "$(direct_arg_count prime --sample-cases=100)" 'prime sample budget'
  assert_eq 1 "$(direct_arg_count dyn-load-prime --sample-cases=50)" 'dynamic sample budget'
done
run_direct_golden explicit-leaf-sample 0 --impls=FULL_IMPL_MATRIX \
  --all-cases --case-coverage=dyn-load-prime:sample --case-seed=fixed
assert_eq 1 "$(direct_arg_count dyn-load-prime --sample-cases=50)" 'leaf sample budget'

for all_policy in --all-cases --case-coverage=goldens:all \
  --case-coverage=dyn-load-prime:all; do
  run_direct_golden explicit-all 0 --impls=FULL_IMPL_MATRIX "$all_policy"
  assert_eq 0 "$(direct_arg_prefix_count dyn-load-prime --sample-cases=)" \
    'explicit all defeats default'
done
run_direct_golden explicit-numeric 0 --impls=FULL_IMPL_MATRIX --case-count=100
for numeric_mode in regular prime dyn-load-prime; do
  assert_eq 1 "$(direct_arg_count "$numeric_mode" --sample-cases=100)" \
    'explicit numeric is not default sample'
done
for all_order in first last; do
  case $all_order in
    first) set -- --all-cases --sample-cases ;;
    last) set -- --sample-cases --all-cases ;;
  esac
  run_direct_golden global-last-wins 0 --impls=FULL_IMPL_MATRIX "$@"
  expected_sample=0
  [ "$all_order" != first ] || expected_sample=1
  assert_eq "$expected_sample" "$(direct_arg_count dyn-load-prime --sample-cases=50)" \
    'last global mode wins'
  run_direct_golden leaf-all-wins 0 --impls=FULL_IMPL_MATRIX \
    --case-coverage=dyn-load-prime:all "$@"
  assert_eq 0 "$(direct_arg_prefix_count dyn-load-prime --sample-cases=)" \
    'leaf all defeats later global sample'
done
run_direct_golden root-all-wins 0 --impls=FULL_IMPL_MATRIX \
  --case-coverage=goldens:all --sample-cases
assert_eq 0 "$(direct_arg_prefix_count dyn-load-prime --sample-cases=)" \
  'root all defeats later global sample'
run_direct_golden leaf-all-last 0 --impls=FULL_IMPL_MATRIX \
  --case-count=7 --case-coverage=dyn-load-prime:sample \
  --case-coverage=dyn-load-prime:all
assert_eq 0 "$(direct_arg_prefix_count dyn-load-prime --sample-cases=)" \
  'last leaf all defeats root numeric and earlier leaf sample'

run_direct_golden full-workflow 0 --impls=FULL_IMPL_MATRIX --all-cases
assert_direct_calls 1 1 1 full-workflow
assert_direct_impl_totals 8 1 1 full-workflow
assert_direct_impl prime kio-prime@js 1 full-workflow
assert_direct_impl dyn-load-prime dyn-load-prime@kio-prime 1 full-workflow
assert_eq 0 "$(direct_arg_prefix_count dyn-load-prime --impl-case-set-file=)" \
  'full dyn cohort is not named-set narrowed'
assert_eq 0 "$(direct_arg_prefix_count dyn-load-prime --sample-cases=)" \
  'full dyn cohort is not sampled'

# I07-I10: exact regular/fixed selection and both registered generic leaves.
run_direct_golden exact-regular 0 --impls=kio@js
assert_direct_calls 1 0 0 exact-regular
assert_direct_impl_totals 1 0 0 exact-regular
assert_direct_impl regular kio@js 1 exact-regular

run_direct_golden exact-fixed 0 --impls=dyn-load-prime@kio-prime
assert_direct_calls 0 0 1 exact-fixed
assert_direct_impl_totals 0 0 1 exact-fixed
assert_direct_impl dyn-load-prime dyn-load-prime@kio-prime 1 exact-fixed

run_direct_golden augment-dyn 0 --impls=kio@js \
  --impl-verification=dyn-load-prime
assert_direct_calls 1 0 1 augment-dyn
assert_direct_impl_totals 1 0 1 augment-dyn
assert_direct_impl regular kio@js 1 augment-dyn
assert_direct_impl dyn-load-prime dyn-load-prime@kio-prime 1 augment-dyn

run_direct_golden augment-prime 0 --impls=kio@js \
  --impl-verification=kio-prime
assert_direct_calls 1 1 0 augment-prime
assert_direct_impl_totals 1 1 0 augment-prime
assert_direct_impl regular kio@js 1 augment-prime
assert_direct_impl prime kio-prime@js 1 augment-prime

# A01-A04: an explicitly targeted inactive leaf is an input error; inherited
# policies stay inert; activation makes the leaf's named set win locally.
assert_prebuild_rejection inactive-target \
  'error: --case-coverage=dyn-load-prime:smoke targets inactive verification scope dyn-load-prime; add --impl-verification=dyn-load-prime or select dyn-load-prime@kio-prime' \
  --impls=kio@js --case-coverage=dyn-load-prime:smoke

run_direct_golden inherited-global 0 --impls=kio@js --sample-cases \
  --case-seed=fixed
assert_direct_calls 1 0 0 inherited-global
assert_eq 1 "$(direct_arg_count regular --sample-cases=100)" \
  'inherited global sample cap'

run_direct_golden inherited-root 0 --impls=kio@js \
  --case-coverage=goldens:1 --case-seed=fixed
assert_direct_calls 1 0 0 inherited-root
assert_eq 1 "$(direct_arg_count regular --sample-cases=1)" \
  'inherited root cap'

run_direct_golden activated-leaf 0 --impls=kio@js \
  --impl-verification dyn-load-prime \
  --case-coverage dyn-load-prime:smoke
assert_direct_calls 1 0 1 activated-leaf
assert_eq 0 "$(direct_arg_prefix_count regular --sample-cases=)" \
  'activated leaf leaves regular whole'
assert_eq 1 \
  "$(direct_arg_count dyn-load-prime "--impl-case-set-file=$direct_manifest")" \
  'activated leaf named manifest'

# C01: global, root, and leaf tiers are independent of argv order. The root
# cap reaches regular and prime; the leaf smoke replaces it only for dyn.
for precedence_order in natural permuted; do
  case $precedence_order in
    natural)
      set -- --sample-cases --case-coverage=goldens:7 \
        --case-coverage=dyn-load-prime:smoke ;;
    permuted)
      set -- --case-coverage=dyn-load-prime:smoke \
        --case-coverage=goldens:7 --sample-cases ;;
  esac
  run_direct_golden "precedence-$precedence_order" 0 --impls=kio@js \
    --impl-verification=kio-prime --impl-verification=dyn-load-prime \
    --case-seed=fixed "$@"
  assert_direct_calls 1 1 1 "precedence-$precedence_order"
  assert_eq 1 "$(direct_arg_count regular --sample-cases=7)" \
    "$precedence_order regular root cap"
  assert_eq 1 "$(direct_arg_count prime --sample-cases=7)" \
    "$precedence_order prime root cap"
  assert_eq 1 \
    "$(direct_arg_count dyn-load-prime "--impl-case-set-file=$direct_manifest")" \
    "$precedence_order dyn leaf set"
  assert_eq 0 "$(direct_arg_prefix_count dyn-load-prime --sample-cases=)" \
    "$precedence_order dyn leaf precedence"
done

# C02: last occurrence wins within one exact leaf.
run_direct_golden same-scope-last-wins 0 \
  --impls=dyn-load-prime@kio-prime \
  --case-coverage=dyn-load-prime:all \
  --case-coverage=dyn-load-prime:smoke
assert_direct_calls 0 0 1 same-scope-last-wins
assert_eq 1 \
  "$(direct_arg_count dyn-load-prime "--impl-case-set-file=$direct_manifest")" \
  'same-scope last named policy'

# Feed the owner's captured case controls and exact selectors to the real
# receiving harness. Fake case bodies record actual impl execution; a separate
# case-binary check records the whole filtered cohort, including the omitted case.
execution_cases=$scratch/execution-cases
mkdir -p "$execution_cases/00_success" "$scratch/execution-bin"
cat >"$scratch/execution-bin/kio" <<'EOF'
#!/bin/sh
case "$*" in 'cache --help'|'cache clear') exit 0 ;; *) exit 97 ;; esac
EOF
cat >"$scratch/execution-bin/runner" <<'EOF'
#!/bin/sh
exit 98
EOF
cat >"$scratch/execution-bin/invariant.sh" <<'EOF'
#!/bin/sh
# ROUTING: case-binary
printf '%s\n' "${PWD##*/}" >>"$KIO_TEST_EXECUTION_CHECKS"
[ "${PWD##*/}" != "${KIO_TEST_FAIL_CASE:-}" ]
EOF
chmod +x "$scratch/execution-bin/"*
execution_index=1
set --
while [ "$execution_index" -le 51 ]; do
  execution_name=$(printf 'case_%02d' "$execution_index")
  execution_case=$execution_cases/00_success/$execution_name
  mkdir "$execution_case"
  cat >"$execution_case/run.sh" <<'EOF'
#!/bin/sh
printf '%s\n' "${PWD##*/}" >>"$KIO_TEST_EXECUTION_IMPLS"
[ ! -f KNOWN_FAILING ]
EOF
  chmod +x "$execution_case/run.sh"
  printf '0\n' >"$execution_case/expected.exit"
  : >"$execution_case/expected.stdout"
  : >"$execution_case/expected.stderr.ignore"
  : >"$execution_case/DYN_LOAD_PRIME"
  set -- "$@" "^00_success/$execution_name\$"
  execution_index=$((execution_index + 1))
done

run_execution_fixture() {
  ref_label=$1 ref_expected=$2
  shift 2
  run_direct_golden "$ref_label" 0 --impls=FULL_IMPL_MATRIX \
    --case-seed=fixed "$@"
  awk -F '\t' '$1 == "ARG" && $2 == "dyn-load-prime" &&
    ($3 ~ /^--sample-cases=/ || $3 ~ /^--case-seed=/ || $3 ~ /^\^/) { print $3 }' \
    "$direct_run_log" >"$scratch/execution.args"
  set --
  while IFS= read -r execution_arg; do set -- "$@" "$execution_arg"; done \
    <"$scratch/execution.args"
  : >"$scratch/execution-impls"
  : >"$scratch/execution-checks"
  set +e
  TMPDIR=$scratch/direct-tmp KIO_CI_SCHEDULE=DISABLE \
    KIO_DEBUG_PROGRESS_INTERVAL=0 \
    KIO_TEST_EXECUTION_IMPLS=$scratch/execution-impls \
    KIO_TEST_EXECUTION_CHECKS=$scratch/execution-checks \
    KIO_TEST_FAIL_CASE=${execution_fail_case:-} \
    sh "$REPO_ROOT/ci/run-tests.sh" --cases-dir="$execution_cases" \
      --cache-base="$scratch/execution-cache" --jobs=1 --compiler-jobs=1 \
      --dyn-load-prime-only \
      --impl-def="name=dyn-load-prime@kio-prime,kio=$scratch/execution-bin/kio,runner=$scratch/execution-bin/runner,target=kio-prime" \
      --check="$scratch/execution-bin/invariant.sh" "$@" \
      >"$scratch/execution-$ref_label.log" 2>&1
  ref_status=$?
  set -e
  if [ "$ref_status" -ne "$ref_expected" ]; then
    cat "$scratch/execution-$ref_label.log" >&2
    printf 'coverage-policy-selftest: execution %s exited %s, expected %s\n' \
      "$ref_label" "$ref_status" "$ref_expected" >&2
    exit 1
  fi
}

run_execution_fixture default 0 -- "$@"
assert_eq 50 "$(wc -l <"$scratch/execution-impls" | tr -d ' ')" '51-case default impl count'
assert_eq 51 "$(wc -l <"$scratch/execution-checks" | tr -d ' ')" '51-case default checks count'
LC_ALL=C sort "$scratch/execution-impls" >"$scratch/default-selection"
LC_ALL=C sort "$scratch/execution-checks" >"$scratch/filtered-selection"
execution_omitted=$(comm -13 "$scratch/default-selection" "$scratch/filtered-selection")
[ -n "$execution_omitted" ] || exit 1
run_execution_fixture repeat 0 -- "$@"
LC_ALL=C sort "$scratch/execution-impls" >"$scratch/repeat-selection"
cmp "$scratch/default-selection" "$scratch/repeat-selection"

run_execution_fixture hosted-ten 0 --case-coverage=dyn-load-prime:10 -- "$@"
assert_eq 10 "$(wc -l <"$scratch/execution-impls" | tr -d ' ')" \
  'one-bucket hosted dynamic impl count'
assert_eq 51 "$(wc -l <"$scratch/execution-checks" | tr -d ' ')" \
  'hosted dynamic checks keep full cohort'
grep -Fq 'sample-cases: cap = 10 case(s) per bucket' \
  "$scratch/execution-hosted-ten.log"
grep -Fq 'sample-cases: seed = fixed' \
  "$scratch/execution-hosted-ten.log"
LC_ALL=C sort "$scratch/execution-impls" >"$scratch/hosted-ten-selection"
run_execution_fixture hosted-ten-replay 0 --case-coverage=dyn-load-prime:10 -- "$@"
LC_ALL=C sort "$scratch/execution-impls" >"$scratch/hosted-ten-replay-selection"
cmp "$scratch/hosted-ten-selection" "$scratch/hosted-ten-replay-selection"

run_execution_fixture all 0 --all-cases -- "$@"
assert_eq 51 "$(wc -l <"$scratch/execution-impls" | tr -d ' ')" '51 exact mandatory impls'
assert_eq 51 "$(wc -l <"$scratch/execution-checks" | tr -d ' ')" '51 exact mandatory checks'
run_execution_fixture single 0 -- "^00_success/$execution_omitted\$"
assert_eq 1 "$(wc -l <"$scratch/execution-impls" | tr -d ' ')" 'exact filter precedes sample'
assert_eq "$execution_omitted" "$(cat "$scratch/execution-impls")" 'exact omitted case now executes'

execution_fail_case=$execution_omitted
run_execution_fixture invariant-red 1 -- "$@"
execution_fail_case=
: >"$execution_cases/00_success/$execution_omitted/KNOWN_FAILING"
run_execution_fixture known-failure 0 -- "$@"
assert_eq 51 "$(wc -l <"$scratch/execution-impls" | tr -d ' ')" 'known failure outside draw is pinned'
assert_eq 50 "$(wc -l <"$scratch/execution-checks" | tr -d ' ')" 'known failure retains check exclusion'

# R04 integration: the owner must discharge every registered fixed impl before
# any build or run boundary; a standalone helper test cannot pin that call.
direct_verification=$direct_lib/verification-scopes.tsv
direct_verification_canonical=$scratch/direct-verification-scopes.canonical
cp "$direct_verification" "$direct_verification_canonical"
awk -F '\t' 'BEGIN { OFS="\t" }
  $1 == "dyn-load-prime" { $5="missing@fixed" }
  { print }
' "$direct_verification_canonical" >"$direct_verification"
assert_prebuild_rejection owner-missing-fixed \
  'verification scope dyn-load-prime names fixed implementation not defined by owner golden-tests.sh: missing@fixed' \
  --impls=kio@js
cp "$direct_verification_canonical" "$direct_verification"
cmp -s "$direct_verification_canonical" "$direct_verification" || {
  printf 'coverage-policy-selftest: direct verification registry was not restored\n' >&2
  exit 1
}

# C03/C04 and empty public values all fail before any fake build boundary.
assert_prebuild_rejection zero-policy 'invalid case-coverage policy: 0' \
  --impls=kio@js --case-coverage=goldens:0
assert_prebuild_rejection padded-zero-policy 'invalid case-coverage policy: 00' \
  --impls=kio@js --case-coverage=goldens:00
assert_prebuild_rejection unknown-named \
  'unknown named case policy no-such-set for scope dyn-load-prime' \
  --impls=dyn-load-prime@kio-prime \
  --case-coverage=dyn-load-prime:no-such-set
assert_prebuild_rejection unknown-verification \
  'unknown implementation-verification scope: no-such-scope' \
  --impls=kio@js --impl-verification=no-such-scope
assert_prebuild_rejection root-as-verification \
  '--impl-verification=goldens requires a fixed-verification scope' \
  --impls=kio@js --impl-verification=goldens
assert_prebuild_rejection empty-verification \
  '--impl-verification requires a value' --impls=kio@js --impl-verification=
assert_prebuild_rejection empty-coverage \
  '--case-coverage requires a value' --impls=kio@js --case-coverage=
assert_prebuild_rejection custom-cache-passthrough \
  '--custom-typed-cache-cohort is internal orchestrator configuration' \
  --impls=kio@js -- --custom-typed-cache-cohort=exec-dyn-load-goldens-v1

printf 'coverage-policy-selftest: ok (default 50/51; hosted 10/51 with replay; full 51/51; exact filters; invariant RED; known-failure pin)\n'
