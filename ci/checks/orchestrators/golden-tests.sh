#!/bin/sh
# shellcheck disable=SC2034 # Capacity-explicitness is read by sourced common.sh.
#
# Build the implementations + verifier and run the goldens corpus
# through `ci/run-tests.sh`, with per-case checks (kio fmt
# idempotence, IS_KIO_PRIME biconditional, Kio' round-trip,
# rlib-cache warm-hit) wired in via run-tests' `--check=` pipeline.
#
# The runner walks `test-data/goldens/` once and emits a worklist of
# `(case, binary)` units. For each unit, it runs `<binary> cache
# clear`, then the source-level checks (fmt-canonical, prime-marker;
# marked `# ROUTING: case-binary`), then each impl on that
# (case, binary) sequentially against the case's real directory.
# Per-impl checks (kio-prime-roundtrip, rlib-cache-second-run-hits)
# fire inside the impl step. Failures are reported alongside the
# case-run outcome for that impl.
#
# Two implementation lists are hardcoded below: $ALL_IMPLS (regular
# Kio compilers, run on every case) and $ALL_PRIME_IMPLS (Kio'-only
# compilers, run only on IS_KIO_PRIME-marked cases via run-tests'
# --prime-only filter). Each list has the tab-separated form
# name<TAB>kio<TAB>runner<TAB>target, one record per line. The
# `target` field names which build target an impl handles, and
# run-tests skips cases whose root package-file `build { ... }`
# block does not declare that target. Adding an implementation =
# adding a record to the appropriate list plus its build step.
#
# Case sampling. Regular and direct Prime goldens run whole by default;
# the dynamic Prime differential samples 50 cases per exit-code bucket.
# `--sample-cases` caps every active group's implementation runs at its own
# budget. A sampled-out golden still runs every
# `# ROUTING: case-binary` check this orchestrator wires. See
# ci/run-tests.sh --sample-cases.
#
# Usage:
#   sh ci/checks/orchestrators/golden-tests.sh \
#     [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] \
#     [--compiler-jobs=<N>] \
#     [--impl-verification=<scope>] [--case-coverage=<scope>:<policy>] \
#     [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] \
#     [-- <run-tests args>...]
#
# --impls=FULL_IMPL_MATRIX
#                 run all configured impls (default).
# --impls=SAMPLE_IMPL
#                 for each case, run exactly one applicable impl,
#                 picked at random per run.
# --impls=<list>  restrict run-tests to the named comma-separated impls.
# --impl-verification=<scope>
#                 augment an explicit list with one registered fixed verifier.
# --case-coverage=<scope>:<policy>
#                 override the goldens root or an active verification leaf;
#                 a leaf wins over the inherited root policy.
# --jobs=<N>      passed through to run-tests as --jobs; default is
#                 run-tests' native available-parallelism default.
# --compiler-jobs=<N>
#                 cap top-level Cargo invocations, compiler-producing Kio
#                 commands, and runner compiler commands; omission uses paced,
#                 best-effort CPU/memory feedback; a numeric value is a fixed cap.
# --all-cases     every filtered golden runs its impls unless an explicit
#                 root or leaf policy narrows coverage.
# --sample-cases  cap regular/direct Prime at $DEFAULT_CASE_COUNT per
#                 exit-code bucket and dynamic Prime at $DYN_CASE_COUNT.
# --case-count=<N>
#                 cap at N per bucket instead.
# --case-seed=<S> pin the draw for reproduction.
#
# Anything after `--` is forwarded verbatim to ci/run-tests.sh.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
OWNER=${0##*/}

# Absolute-path source so the helpers resolve from any cwd; shellcheck
# can't follow the dynamic $SCRIPT_DIR path (SC1091).
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"
COVERAGE_POLICY_DIR=$SCRIPT_DIR/lib
export COVERAGE_POLICY_DIR
# shellcheck source=/dev/null
. "$COVERAGE_POLICY_DIR/coverage-policy.sh"

TAB=$(printf '\t')

set_default_build_cache_size

# Per-exit-code-bucket cap when this corpus samples. 00_success is the
# only bucket the cap really bites: its goldens reach a backend, while
# every error bucket is smaller than this and so runs whole.
DEFAULT_CASE_COUNT=100
DYN_CASE_COUNT=50

# Regular and direct Prime goldens run whole by default. The dynamic
# differential has its own sample budget; explicit case policies override it.
#
# Consumed across the source boundary by lib/common.sh (SC2034).
# shellcheck disable=SC2034
CASE_MODE=all
# shellcheck disable=SC2034
CASE_COUNT=$DEFAULT_CASE_COUNT
# shellcheck disable=SC2034
CASE_SEED=
global_case_policy=

# Configured implementations: one record per line, tab-separated as
# name<TAB>kio<TAB>runner<TAB>target. Names must not contain
# whitespace or commas. The `target` slot selects which build
# target the impl handles; run-tests skips cases whose root build
# file does not declare that target.
#
# $ALL_IMPLS — regular Kio compilers, run against the full corpus.
# $ALL_PRIME_IMPLS — Kio'-only compilers, run only on IS_KIO_PRIME-marked
#                   cases via run-tests' --prime-only filter. Asserts
#                   that the marked cases really are Kio'-shaped from
#                   that compiler's perspective.
#
# IMPL-LIST PARITY RULE — every test harness that defines a run-tests
# impl list keeps the same shape across harnesses: every kio compiler /
# backend pair the repo ships shows up in each harness's $ALL_IMPLS
# unless that harness deliberately scopes itself narrower (e.g.,
# `ci/checks/orchestrators/kiodoc-tests.sh` is js-only because doc snippets don't
# exercise the rust backend). When a new compiler or backend is added,
# update every harness's $ALL_IMPLS in the same change. See
# `TESTING.md` § Harness impl-list parity for the rule.
ALL_IMPLS="kio@js${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-js${TAB}js
kio@ts${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-ts${TAB}ts
kio@python${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-python${TAB}python
kio@java${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-java${TAB}java
kio@rust${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-rust${TAB}rust
kio@go${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-go${TAB}go
kio@swift${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-swift${TAB}swift
kio@haskell${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-haskell${TAB}haskell"
# The compiler slot is replaced with the isolated Prime-only binary after the
# orchestrator builds it below. Keeping a sentinel here lets option validation
# and --help inspect the impl list before the temporary build directory exists.
ALL_PRIME_IMPLS="kio-prime@js${TAB}__prime_only_kio__${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-js${TAB}js"
# $ALL_DYN_LOAD_PRIME_IMPLS — the dyn_load_prime dyn-load-prime differential. Run only
# on DYN_LOAD_PRIME-marked cases (via run-tests'
# --dyn-load-prime-only filter):
# each case's emitted Kio' image (target=kio-prime) is loaded through
# dyn_load_prime. `compile-only` stops after package loading,
# `construct-only` instantiates the exact empty-host package, main protocols
# instantiate their selected exact host contract and invoke `main` in its exact
# declaring module, and supported export protocols drive their scripted
# surface. Each
# route is checked against its compiled-runner counterpart. The compiler is the regular `kio` (it
# produces the kio-prime image); the runner is
# kio-test-runner-dyn-load-prime.
ALL_DYN_LOAD_PRIME_IMPLS="dyn-load-prime@kio-prime${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-dyn-load-prime${TAB}kio-prime"

ONLY=
JOBS=
if [ "${KIO_CI_SCHEDULE_COMPILER_JOBS+x}" = x ]; then
  COMPILER_JOBS=$KIO_CI_SCHEDULE_COMPILER_JOBS
  COMPILER_JOBS_EXPLICIT=1
else
  COMPILER_JOBS=adaptive
  COMPILER_JOBS_EXPLICIT=0
fi
SAMPLE_IMPL=0
IMPL_VERIFICATION_SCOPES=
SCOPED_CASE_POLICIES=

while [ $# -gt 0 ]; do
  case "$1" in
    --impls=*) apply_impls_spec "${1#--impls=}" ;;
    --impls)
      shift
      [ $# -gt 0 ] || { printf 'error: --impls requires a value\n' >&2; exit 2; }
      apply_impls_spec "$1"
      ;;
    --impl-verification=*)
      [ -n "${1#--impl-verification=}" ] || { printf 'error: --impl-verification requires a value\n' >&2; exit 2; }
      IMPL_VERIFICATION_SCOPES="${IMPL_VERIFICATION_SCOPES}${IMPL_VERIFICATION_SCOPES:+
}${1#--impl-verification=}"
      ;;
    --impl-verification)
      shift
      [ $# -gt 0 ] || { printf 'error: --impl-verification requires a value\n' >&2; exit 2; }
      IMPL_VERIFICATION_SCOPES="${IMPL_VERIFICATION_SCOPES}${IMPL_VERIFICATION_SCOPES:+
}$1"
      ;;
    --case-coverage=*)
      [ -n "${1#--case-coverage=}" ] || { printf 'error: --case-coverage requires a value\n' >&2; exit 2; }
      SCOPED_CASE_POLICIES="${SCOPED_CASE_POLICIES}${SCOPED_CASE_POLICIES:+
}${1#--case-coverage=}"
      ;;
    --case-coverage)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-coverage requires a value\n' >&2; exit 2; }
      SCOPED_CASE_POLICIES="${SCOPED_CASE_POLICIES}${SCOPED_CASE_POLICIES:+
}$1"
      ;;
    --jobs=*) JOBS=${1#--jobs=} ;;
    --jobs)
      shift
      [ $# -gt 0 ] || { printf 'error: --jobs requires a value\n' >&2; exit 2; }
      JOBS=$1
      ;;
    --compiler-jobs=*)
      COMPILER_JOBS=${1#--compiler-jobs=}
      COMPILER_JOBS_EXPLICIT=1
      ;;
    --compiler-jobs)
      shift
      [ $# -gt 0 ] || { printf 'error: --compiler-jobs requires a value\n' >&2; exit 2; }
      COMPILER_JOBS=$1
      COMPILER_JOBS_EXPLICIT=1
      ;;
    --all-cases) apply_all_cases; global_case_policy=all ;;
    --sample-cases) apply_sample_cases; global_case_policy=sample ;;
    --case-count=*) apply_case_count "${1#--case-count=}"; global_case_policy=$CASE_COUNT ;;
    --case-count)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-count requires a value\n' >&2; exit 2; }
      apply_case_count "$1"
      global_case_policy=$CASE_COUNT
      ;;
    --case-seed=*) apply_case_seed "${1#--case-seed=}" ;;
    --case-seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
      apply_case_seed "$1"
      ;;
    -h|--help)
      regular=$(printf '%s\n' "$ALL_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
      prime=$(printf '%s\n' "$ALL_PRIME_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
      dyn_load_prime=$(printf '%s\n' "$ALL_DYN_LOAD_PRIME_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
      cat <<EOF
Usage: sh $0 [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--impl-verification=<scope>] [--case-coverage=<scope>:<policy>] [--jobs=<N>] [--compiler-jobs=<N>] [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] [-- <run-tests args>...]

Build the configured Kio implementations + the Kio' verifier, then
run ci/run-tests.sh over test-data/goldens with per-case checks
wired in (kio fmt idempotence, IS_KIO_PRIME biconditional).

Regular impls are run on the full corpus. Prime impls are run only on
IS_KIO_PRIME-marked cases (via run-tests' --prime-only filter), so a
Kio'-only compiler can assert those cases really are Kio'. Dyn-load-prime
impls are run only on DYN_LOAD_PRIME-marked cases (via
--dyn-load-prime-only):
dyn_load_prime loads each case's Kio' image, then follows the shared
protocol's exact execution mode: load-only, construct-only, an exact
module-qualified main, or a supported export-surface script.

--impls=FULL_IMPL_MATRIX runs all configured impls (default).
--impls=SAMPLE_IMPL forwards to run-tests so each case runs on one
applicable impl, picked at random per run. Use for local iteration.
--impls=<list> restricts to the named impls (comma-separated).
The impl groups are invoked as separate run-tests.sh runs, each with
its own summary. See TESTING.md § Local iteration.

--impl-verification=<scope> augments an explicit list with a registered
fixed verifier owned by this orchestrator. SAMPLE_IMPL and FULL_IMPL_MATRIX
already include fixed verifiers, so repeated requests deduplicate.
--case-coverage=<scope>:<all|sample|N|named-set> overrides the goldens root
or one registered verification leaf. A leaf policy wins over its inherited
root policy, which wins over global flags; defaults apply only below those
explicit policies. Explicitly targeting an inactive leaf is an error; an inherited
root or global policy may remain inert for an inactive leaf.

--jobs is forwarded to run-tests as --jobs; default is run-tests'
auto = native scheduler available parallelism.
--compiler-jobs caps top-level Cargo invocations, compiler-producing Kio
commands, and actual native compiler commands across participating worktrees;
omission uses paced, best-effort CPU/memory feedback; a numeric value is a fixed cap.

Case coverage (which GOLDENS run their impls): regular and direct Prime
run whole by default; dynamic Prime samples $DYN_CASE_COUNT per exit-code bucket.
--all-cases     every filtered golden runs, unless an explicit root/leaf
                policy overrides it. FULL_IMPL_MATRIX alone does not do this.
--sample-cases  cap regular/direct Prime impl runs at $DEFAULT_CASE_COUNT per bucket
                and dynamic Prime at $DYN_CASE_COUNT. Scoped :sample uses the same
                budget for each active group. Explicit leaf policies win.
--case-count=<N>
                cap at N per bucket instead.
--case-seed=<S> pin the draw. Default: the GitHub Actions run context,
                else UTC epoch seconds. run-tests prints the effective
                seed and the selected goldens.

A golden narrowed out of the impl tier still runs every
\`# ROUTING: case-binary\` check
(fmt idempotence, dependency-tree canonicality, the IS_KIO_PRIME
biconditional): sampling, numeric caps, and named sets scope the build+run
tier only. KNOWN_FAILING reproducers remain in that tier even when outside a
named set.

Anything after \`--\` is forwarded verbatim to ci/run-tests.sh.

Regular impls:    ${regular}
Prime impls:      ${prime}
Dyn-load-prime impls: ${dyn_load_prime}
EOF
      exit 0
      ;;
    --) shift; break ;;
    -*) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    *) printf 'error: unknown argument: %s (case-name filters go after --)\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

# After this loop, "$@" holds the passthrough args (everything after --).
resolve_compiler_jobs

# Save the passthrough args so each run-tests.sh invocation can prepend
# them to its own argv.
passthrough_count=$#
init_orchestrator_tmp golden-tests
TMP_ROOT=$ORCHESTRATOR_TMP
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_golden_tests() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$TMP_ROOT"
  exit "$cleanup_status"
}
trap cleanup_golden_tests EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
coverage_policy_validate_registry "$SCRIPT_DIR"
PASSTHROUGH_FILE="$TMP_ROOT/passthrough.args"
: >"$PASSTHROUGH_FILE"
i=0
while [ "$i" -lt "$passthrough_count" ]; do
  case "$1" in
    --custom-typed-cache-cohort|--custom-typed-cache-cohort=*)
      coverage_policy_die \
        "--custom-typed-cache-cohort is internal orchestrator configuration"
      ;;
  esac
  printf '%s\n' "$1" >>"$PASSTHROUGH_FILE"
  shift
  i=$((i + 1))
done

# Direct callers opt fixed verification leaves in explicitly. Augment the
# public implementation list before the existing union validator/filter.
printf '%s\n' "$IMPL_VERIFICATION_SCOPES" >"$TMP_ROOT/requested-verification-scopes"
while IFS= read -r verification_scope; do
  [ -n "$verification_scope" ] || continue
  if ! fixed_impl=$(coverage_policy_fixed_impl_for_scope "$verification_scope" 2>/dev/null); then
    if coverage_policy_owner_for_root "$verification_scope" >/dev/null 2>&1; then
      coverage_policy_die "--impl-verification=$verification_scope requires a fixed-verification scope"
    fi
    coverage_policy_die "unknown implementation-verification scope: $verification_scope"
  fi
  [ "$(coverage_policy_owner_for_scope "$verification_scope")" = "$OWNER" ] ||
    coverage_policy_die "implementation-verification scope $verification_scope belongs to another owner"
  printf '%s\n' "$fixed_impl"
done <"$TMP_ROOT/requested-verification-scopes" >"$TMP_ROOT/requested-fixed-impls.unsorted"
LC_ALL=C sort -u "$TMP_ROOT/requested-fixed-impls.unsorted" >"$TMP_ROOT/requested-fixed-impls"
if [ -s "$TMP_ROOT/requested-fixed-impls" ] && [ -n "$ONLY" ]; then
  while IFS= read -r fixed_impl; do
    case ",$ONLY," in *",$fixed_impl,"*) ;; *) ONLY=$ONLY,$fixed_impl ;; esac
  done <"$TMP_ROOT/requested-fixed-impls"
fi

case_policies_file=$TMP_ROOT/case-policies.tsv
: >"$case_policies_file"
printf '%s\n' "$SCOPED_CASE_POLICIES" >"$TMP_ROOT/requested-case-policies"
while IFS= read -r coverage_spec; do
  [ -n "$coverage_spec" ] || continue
  case "$coverage_spec" in *:*) ;; *) coverage_policy_die "invalid --case-coverage=$coverage_spec; expected <scope>:<policy>" ;; esac
  coverage_scope=${coverage_spec%%:*}; coverage_value=${coverage_spec#*:}
  coverage_policy_validate_identifier "$coverage_scope" || coverage_policy_die "invalid case-coverage scope: $coverage_scope"
  coverage_policy_validate_policy "$coverage_value" || coverage_policy_die "invalid case-coverage policy: $coverage_value"
  if [ "$coverage_scope" != "$(coverage_policy_root_for_owner "$OWNER")" ] &&
     [ "$(coverage_policy_owner_for_scope "$coverage_scope" 2>/dev/null || :)" != "$OWNER" ]; then
    coverage_policy_die "unknown case-coverage scope: $coverage_scope"
  fi
  case "$coverage_value" in
    all|sample) ;;
    *[!0-9]*) coverage_policy_named_set_registered "$coverage_scope" "$coverage_value" ||
      coverage_policy_die "unknown named case policy $coverage_value for scope $coverage_scope" ;;
  esac
  printf '%s\t%s\n' "$coverage_scope" "$coverage_value"
done <"$TMP_ROOT/requested-case-policies" >"$case_policies_file"

last_policy() {
  awk -F '\t' -v scope="$1" '$1 == scope { value=$2 } END { if (value != "") print value }' "$case_policies_file"
}

owner_root=$(coverage_policy_root_for_owner "$OWNER")
root_policy=$(last_policy "$owner_root")

effective_case_policy() {
  ecp_scope=$1
  ecp_policy=$(last_policy "$ecp_scope")
  [ -n "$ecp_policy" ] || ecp_policy=$root_policy
  [ -n "$ecp_policy" ] || ecp_policy=$global_case_policy
  if [ -z "$ecp_policy" ]; then
    case $ecp_scope in
      dyn-load-prime) ecp_policy=sample ;;
      *) ecp_policy=all ;;
    esac
  fi
  printf '%s\n' "$ecp_policy"
}

# Validate --impls=<list> against the union of all impl lists before any
# filtering, so a typo in one bucket does not silently fall through. This
# harness has three impl lists, so it keeps a union validator local
# rather than lib/common.sh's single-list validate_impls_against_list.
# NB: multi-line strings are passed to awk via ENVIRON[] (see
# lib/common.sh); BSD awk on macOS rejects newlines in `-v` values.
if [ -n "$ONLY" ]; then
  unknown=$(IMPL_REGULAR=$ALL_IMPLS IMPL_PRIME=$ALL_PRIME_IMPLS \
    IMPL_DYN_LOAD_PRIME=$ALL_DYN_LOAD_PRIME_IMPLS IMPL_ONLY=$ONLY \
    awk -F"$TAB" '
    BEGIN {
      collect(ENVIRON["IMPL_REGULAR"])
      collect(ENVIRON["IMPL_PRIME"])
      collect(ENVIRON["IMPL_DYN_LOAD_PRIME"])
      only = ENVIRON["IMPL_ONLY"]
      missing = ""
      m = split(only, want, ",")
      for (i = 1; i <= m; i++) {
        if (want[i] == "") continue
        if (!(want[i] in known)) missing = missing " " want[i]
      }
      print missing
    }
    function collect(blob,    n, lines, i, rec) {
      n = split(blob, lines, "\n")
      for (i = 1; i <= n; i++) {
        if (lines[i] == "") continue
        split(lines[i], rec, "\t")
        known[rec[1]] = 1
      }
    }
  ')
  if [ -n "$unknown" ]; then
    regular=$(printf '%s\n' "$ALL_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
    prime=$(printf '%s\n' "$ALL_PRIME_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
    dyn_load_prime=$(printf '%s\n' "$ALL_DYN_LOAD_PRIME_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
    printf 'error: --impls: unknown impl(s):%s (regular: %s; prime: %s; dyn-load-prime: %s)\n' \
      "$unknown" "$regular" "$prime" "$dyn_load_prime" >&2
    exit 2
  fi
fi

printf '%s\n' "$ALL_IMPLS" >"$TMP_ROOT/owner-regular-impls"
printf '%s\n' "$ALL_PRIME_IMPLS" >"$TMP_ROOT/owner-prime-impls"
printf '%s\n' "$ALL_DYN_LOAD_PRIME_IMPLS" >"$TMP_ROOT/owner-dyn-impls"
awk -F "$TAB" 'NF { print $1 }' \
  "$TMP_ROOT/owner-regular-impls" "$TMP_ROOT/owner-prime-impls" \
  "$TMP_ROOT/owner-dyn-impls" >"$TMP_ROOT/owner-defined-impls"
coverage_policy_validate_owner_fixed_impls "$OWNER" "$TMP_ROOT/owner-defined-impls"

SELECTED_REGULAR=$(filter_impls "$ALL_IMPLS")
SELECTED_PRIME=$(filter_impls "$ALL_PRIME_IMPLS")
SELECTED_DYN_LOAD_PRIME=$(filter_impls "$ALL_DYN_LOAD_PRIME_IMPLS")
printf '%s\n%s\n%s\n' "$SELECTED_REGULAR" "$SELECTED_PRIME" \
  "$SELECTED_DYN_LOAD_PRIME" >"$TMP_ROOT/selected-impls"

scoped_sampling=0
if awk -F "$TAB" '$2 == "sample" || $2 ~ /^[1-9][0-9]*$/ { found=1 }
    END { exit !found }' "$case_policies_file"; then
  scoped_sampling=1
fi
if [ "$CASE_MODE" = sample ] || [ "$scoped_sampling" = 1 ] ||
   [ -n "$SELECTED_DYN_LOAD_PRIME" ]; then
  saved_case_mode=$CASE_MODE
  CASE_MODE=sample
  resolve_case_seed
  CASE_MODE=$saved_case_mode
fi

while IFS=$TAB read -r targeted_scope targeted_policy; do
  targeted_fixed=$(coverage_policy_fixed_impl_for_scope "$targeted_scope" 2>/dev/null || :)
  [ -n "$targeted_fixed" ] || continue
  if ! awk -F "$TAB" -v impl="$targeted_fixed" \
       '$1 == impl { found=1; exit } END { exit !found }' "$TMP_ROOT/selected-impls"; then
    printf 'error: --case-coverage=%s:%s targets inactive verification scope %s; add --impl-verification=%s or select %s\n' \
      "$targeted_scope" "$targeted_policy" "$targeted_scope" "$targeted_scope" "$targeted_fixed" >&2
    exit 2
  fi
done <"$case_policies_file"

# Derive each fixed verifier's canonical cohort before any build. Named-set
# classification is data-driven; the owner supplies only its ordinary marker
# predicates.
named_scope_index=$TMP_ROOT/named-scope-index.tsv
: >"$named_scope_index"
prime_scope_impl=${ALL_PRIME_IMPLS%%"$TAB"*}
prime_scope_name=$(coverage_policy_verification_scope_for_impl "$prime_scope_impl")
dyn_scope_impl=${ALL_DYN_LOAD_PRIME_IMPLS%%"$TAB"*}
dyn_scope_name=$(coverage_policy_verification_scope_for_impl "$dyn_scope_impl")
prime_scope_cases=$TMP_ROOT/named-scope-prime.cases
dyn_scope_cases=$TMP_ROOT/named-scope-dyn.cases
scope_case_dirs=$TMP_ROOT/all-scope-case-dirs
find "$REPO_ROOT/test-data/goldens" -name expected.exit -type f -exec dirname {} \; \
  >"$scope_case_dirs"
: >"$prime_scope_cases.unsorted"
: >"$dyn_scope_cases.unsorted"
while IFS= read -r scope_case_dir; do
  [ -n "$scope_case_dir" ] || continue
  scope_case=${scope_case_dir#"$REPO_ROOT/test-data/goldens"/}
  if [ -f "$scope_case_dir/IS_KIO_PRIME" ] && [ ! -f "$scope_case_dir/SKIP_KIO_PRIME_RUN" ]; then
    printf '%s\n' "$scope_case" >>"$prime_scope_cases.unsorted"
  fi
  if [ -f "$scope_case_dir/DYN_LOAD_PRIME" ]; then
    printf '%s\n' "$scope_case" >>"$dyn_scope_cases.unsorted"
  fi
done <"$scope_case_dirs"
LC_ALL=C sort "$prime_scope_cases.unsorted" >"$prime_scope_cases"
LC_ALL=C sort "$dyn_scope_cases.unsorted" >"$dyn_scope_cases"
printf '%s\t%s\n%s\t%s\n' "$prime_scope_name" "$prime_scope_cases" \
  "$dyn_scope_name" "$dyn_scope_cases" >"$named_scope_index"

while IFS=$TAB read -r named_scope named_policy _named_manifest _named_class _named_min _named_overlap; do
  [ "$named_scope" = scope ] && continue
  [ "$(last_policy "$named_scope")" = "$named_policy" ] || continue
  named_canonical=$(awk -F "$TAB" -v scope="$named_scope" '$1 == scope { print $2; exit }' "$named_scope_index")
  [ -n "$named_canonical" ] || coverage_policy_die "named set $named_scope:$named_policy has no canonical verification cohort"
  coverage_policy_validate_named_set "$named_scope" "$named_policy" \
    "$named_canonical" "$REPO_ROOT/test-data/goldens"
done <"$COVERAGE_NAMED_SETS"

# Build phase: every binary needed by any bucket comes from the
# same `cargo build` against the kio-rs crate (which
# produces both `kio` and `kio-prime`), so building once covers
# all. The case-pattern matches any slot whose name has the
# `<binary>@<target>` shape: regular kio@<target> implementations,
# kio-prime@js in the prime bucket, and dyn-load-prime@kio-prime in the
# dyn-load-prime bucket (its compiler is the regular `kio`).
needs_compiler_build=0
case "$SELECTED_REGULAR$SELECTED_PRIME$SELECTED_DYN_LOAD_PRIME" in
  *kio@*|*kio-prime@*|*dyn-load-prime@*) needs_compiler_build=1 ;;
esac
if [ "$needs_compiler_build" = 1 ]; then
  build_corpus_tool_binary \
    kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$TMP_ROOT/kio" \
    --all-features --bins
  KIO_BIN="$TMP_ROOT/kio"
  SELECTED_REGULAR=$(printf '%s\n' "$SELECTED_REGULAR" | awk -F"$TAB" -v OFS="$TAB" \
    -v original="$REPO_ROOT/kio-rs/target/debug/kio" -v kio="$KIO_BIN" '
      NF > 0 { if ($2 == original) $2 = kio; print }
    ')
  SELECTED_DYN_LOAD_PRIME=$(printf '%s\n' "$SELECTED_DYN_LOAD_PRIME" | awk -F"$TAB" -v OFS="$TAB" \
    -v original="$REPO_ROOT/kio-rs/target/debug/kio" -v kio="$KIO_BIN" '
      NF > 0 { if ($2 == original) $2 = kio; print }
    ')
fi

KIO_PRIME_BIN=
if [ -n "$SELECTED_REGULAR$SELECTED_PRIME" ]; then
  build_corpus_tool_binary \
    kio-prime "$REPO_ROOT/kio-rs" kio-prime "$TMP_ROOT/kio-prime" \
    --no-default-features --features prime,cli,parallel --bin kio-prime
  KIO_PRIME_BIN="$TMP_ROOT/kio-prime"
  if [ -n "$SELECTED_PRIME" ]; then
    SELECTED_PRIME=$(printf '%s\n' "$SELECTED_PRIME" | awk -F"$TAB" -v OFS="$TAB" -v kio="$KIO_PRIME_BIN" '
      NF > 0 { $2 = kio; print }
    ')
  fi
fi

build_corpus_tool_binary \
  kio-prime-check "$REPO_ROOT/ci/infra/kio-prime-check-rs" \
  kio-prime-check "$TMP_ROOT/kio-prime-check"

# Build only the runner bins selected by --impls. Each bin builds with
# only its own feature so per-bin dead_code analysis stays honest (see
# ci/checks/hygiene/kio-test-runner-rs.sh for context).
build_runner_feature() {
  feature=$1
  ( cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --no-default-features --features "$feature" )
}

selected_runner_targets=$(
  printf '%s\n%s\n' "$SELECTED_REGULAR" "$SELECTED_PRIME" |
    awk -F"$TAB" 'NF > 0 { print $4 }'
)
for target in js ts python java rust go swift haskell; do
  if printf '%s\n' "$selected_runner_targets" | grep -qx "$target"; then
    build_runner_feature "$target"
  fi
done
if [ -n "$SELECTED_DYN_LOAD_PRIME" ]; then
  build_runner_feature dyn-load-prime
fi

# Build the dyn-load-prime driver once. The kio-test-runner-dyn-load-prime
# runner drives this compiled JS module (the dyn_load_prime package plus a
# driver module) as a dyn_load_prime host to load and drive each
# DYN_LOAD_PRIME-marked case's Kio' image; the path is exported to the runner via
# KIO_DYN_LOAD_PRIME_DRIVER_JS. Built only when the dyn-load-prime impl group is
# selected (so a --impls=<list> that excludes it skips the cost).
DYN_LOAD_PRIME_DRIVER_PUBLICATION="$REPO_ROOT/.kio-cache/goldens/dyn-load-prime-driver"
if [ -n "$SELECTED_DYN_LOAD_PRIME" ]; then
  KIO_DYN_LOAD_PRIME_DRIVER_JS=$(
    sh "$REPO_ROOT/ci/infra/kio-test-runner-rs/dyn-load-prime-driver/build-driver.sh" \
      "$KIO_BIN" \
      "$REPO_ROOT/test-data/poc/dyn_load_prime/workdir" \
      "$DYN_LOAD_PRIME_DRIVER_PUBLICATION"
  )
  export KIO_DYN_LOAD_PRIME_DRIVER_JS
fi

KIO_PRIME_CHECK_BIN="$TMP_ROOT/kio-prime-check"
export KIO_PRIME_CHECK_BIN

cd "$REPO_ROOT"

# Build a run-tests.sh argv vector for one impl list and invoke it.
# Args: $1 = impl-list (multi-line records), $2 = "regular", "prime", or
# "dyn-load-prime". Passthrough args and --jobs are added uniformly; the
# prime invocation adds --prime-only and the dyn-load-prime invocation
# adds --dyn-load-prime-only.
# --dyn-load-prime-only to gate the case set.
run_tests() {
  rt_impls=$1
  rt_mode=$2
  rt_scope=$3

  if [ -z "$rt_impls" ]; then
    return 0
  fi

  set -- "--cases-dir=test-data/goldens" "--cache-base=$(shared_cache_base goldens)"
  if [ "$rt_mode" = regular ]; then
    set -- "$@" "--custom-typed-cache-cohort=exec-dyn-load-goldens-v1"
  fi
  echo "$rt_impls" | while IFS=$TAB read -r impl_name impl_kio impl_runner impl_target; do
    [ -n "$impl_name" ] || continue
    # Translate each canonical target into explicit non-executable cache-kind
    # metadata; the per-case check never recovers it from an executable path.
    case "$impl_target" in
      rust) rt_cache_kind=rlib ;;
      go|haskell|swift) rt_cache_kind=$impl_target ;;
      *) rt_cache_kind= ;;
    esac
    if [ "$rt_mode" = dyn-load-prime ]; then
      printf -- '--impl-def=name=%s,kio=%s,runner=%s,target=%s,runner-cache-kind=%s\n' \
        "$impl_name" "$impl_kio" "$impl_runner" "$impl_target" "$rt_cache_kind"
    else
      printf -- '--impl-def=name=%s,kio=%s,runner=%s,target=%s,prime-kio=%s,runner-cache-kind=%s\n' \
        "$impl_name" "$impl_kio" "$impl_runner" "$impl_target" "$KIO_PRIME_BIN" "$rt_cache_kind"
    fi
  done >"$TMPDIR_RT_ARGS"
  while IFS= read -r arg; do
    [ -n "$arg" ] || continue
    set -- "$@" "$arg"
  done <"$TMPDIR_RT_ARGS"
  # The dyn-load-prime group's verification IS the differential (the
  # interpreter's stdout / exit vs the case's expected.*), so it runs
  # only the source-level checks. The impl-routed kio-prime-roundtrip
  # artifact differential and the
  # rust-specific rlib-cache check don't apply to the interpreter.
  if [ "$rt_mode" = dyn-load-prime ]; then
    set -- "$@" \
      "--check=$REPO_ROOT/ci/checks/per-case/fmt-canonical.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/dep-canonical.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/prime-marker.sh"
  else
    set -- "$@" \
      "--check=$REPO_ROOT/ci/checks/per-case/fmt-canonical.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/dep-canonical.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/prime-marker.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/kio-prime-roundtrip.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/rlib-cache-second-run-hits.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/warm-recheck-stable.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/tsc-strict.sh" \
      "--check=$REPO_ROOT/ci/checks/per-case/pyright-strict.sh"
  fi
  if [ -n "$JOBS" ]; then
    set -- "$@" "--jobs=$JOBS"
  fi
  if [ "$COMPILER_JOBS" != adaptive ]; then
    set -- "$@" "--compiler-jobs=$COMPILER_JOBS"
  fi
  if [ "$rt_mode" = prime ]; then
    set -- "$@" "--prime-only"
  fi
  if [ "$rt_mode" = dyn-load-prime ]; then
    set -- "$@" "--dyn-load-prime-only"
  fi
  if [ "$SAMPLE_IMPL" = 1 ]; then
    set -- "$@" "--impls=SAMPLE_IMPL"
  fi
  # Each impl group is its own run-tests invocation over its own case set
  # (regular = the whole corpus; prime / dyn-load-prime = their marked
  # subsets), so the per-bucket cap applies within each group's set.
  # resolve_case_seed pinned one seed for all three before the first
  # invocation, so a single --case-seed replays every group's draw.
  rt_policy=$(effective_case_policy "$rt_scope")
  rt_sample_count=$DEFAULT_CASE_COUNT
  [ "$rt_mode" != dyn-load-prime ] || rt_sample_count=$DYN_CASE_COUNT
  case "$rt_policy" in
    all) ;;
    sample) set -- "$@" "--sample-cases=$rt_sample_count" "--case-seed=$CASE_SEED" ;;
    *[!0-9]*)
      named_file=$(coverage_policy_named_set_file "$rt_scope" "$rt_policy" 2>/dev/null) ||
        coverage_policy_die "unknown named case policy $rt_policy for scope $rt_scope"
      set -- "$@" "--impl-case-set-file=$named_file"
      ;;
    *) set -- "$@" "--sample-cases=$rt_policy" "--case-seed=$CASE_SEED" ;;
  esac
  while IFS= read -r arg; do
    set -- "$@" "$arg"
  done <"$PASSTHROUGH_FILE"

  printf '\n========== run-tests (%s) ==========\n' "$rt_mode"
  sh ci/run-tests.sh "$@"
}

TMPDIR_RT_ARGS="$TMP_ROOT/run-tests.args"

overall_status=0
run_tests "$SELECTED_REGULAR" regular "$owner_root" || overall_status=$?
prime_impl_name=${ALL_PRIME_IMPLS%%"$TAB"*}
prime_scope=$(coverage_policy_verification_scope_for_impl "$prime_impl_name")
dyn_impl_name=${ALL_DYN_LOAD_PRIME_IMPLS%%"$TAB"*}
dyn_scope=$(coverage_policy_verification_scope_for_impl "$dyn_impl_name")
run_tests "$SELECTED_PRIME" prime "$prime_scope" || overall_status=$?
run_tests "$SELECTED_DYN_LOAD_PRIME" dyn-load-prime "$dyn_scope" || overall_status=$?
exit "$overall_status"
