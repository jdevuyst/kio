#!/bin/sh
#
# Run every CI check in parallel; report task starts, completions, and —
# as each task fails — that task's failure diagnostics live (the same
# report emitted at the end), then print buffered logs plus a per-task
# summary at the end. --no-live suppresses the live stream. The caller
# must choose implementation coverage explicitly with SAMPLE_IMPL,
# FULL_IMPL_MATRIX, or impl-name arguments.
# Passing runs delete the temporary task logs by default; failing runs
# repeat concise diagnostics from failed tasks before cleanup.

# Self-name for log lines. Single source of truth so messages stay
# consistent and grep patterns like "$PROG: DONE" remain stable.
PROG=$0
#
# Convenience entrypoint for local development and the Linux selected
# CI gate. macOS / Windows CI runs a narrower portability subset
# directly from the workflow.
#
# Required tools match what .github/workflows/ci.yml installs; consult
# that file for current versions and install commands.
#
# ci/all.sh exports shared admission for leaf work and compiler producers.
# Corpus workers consume work slots; top-level Cargo invocations consume
# compiler permits through ci/cargo.sh. We don't pre-build here — each script
# handles its own build.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
CPU_SUM="$SCRIPT_DIR/infra/ci-time-cpu-sum.sh"
COVERAGE_POLICY_DIR=$SCRIPT_DIR/checks/orchestrators/lib
export COVERAGE_POLICY_DIR
# shellcheck source=/dev/null
. "$COVERAGE_POLICY_DIR/coverage-policy.sh"

IMPLS_SPEC=
GEN_COUNT=
GEN_SEED=
KEEP_LOGS=
LOG_DIR=
JOBS=
COMPILER_JOBS=adaptive
COMPILER_JOBS_EXPLICIT=0
LIVE=1

# Case coverage — how many CASES of each corpus run their impls. Distinct
# from implementation coverage (SAMPLE_IMPL / FULL_IMPL_MATRIX / impl
# names), which chooses how many IMPLS each selected case runs on. The
# two axes compose, and no flag on either is spelled as a bare "sample":
# anything naming `case` cuts cases, anything naming `impl` cuts impls.
#
# Unset, each owner applies its own default: regular/direct Prime goldens,
# emissions, and poc run whole; dynamic Prime, castles, and contrib sample.
# --sample-cases samples all five corpora — what the GitHub gate passes,
# because the whole corpus does not fit in its budget. --all-cases runs all
# five whole. A per-corpus override wins over both.
CASES_MODE=
CASE_SEED=
IMPL_VERIFICATION_SCOPES=
SCOPED_CASE_POLICIES=
TAB=$(printf '\t')

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      cat <<EOF
Usage: sh $0 SAMPLE_IMPL|FULL_IMPL_MATRIX|<name>... [--sample-cases|--all-cases] [--impl-verification=<scope>] [--case-coverage=<scope>:<all|sample|N|named-set>] [--case-seed=<S>] [--gen-count=<N>] [--gen-seed=<N>] [--jobs=<N>] [--compiler-jobs=<N>] [--keep-logs[=<DIR>]] [--no-live]

Run every CI check in parallel; print task starts, completions, and —
as each task fails — that task's failure diagnostics live (the same
report emitted at the end), then buffered logs plus a per-task summary
at the end. --no-live suppresses the live stream (failures still appear
in the end-of-run report).
Leaf work is admitted through a shared scheduler rooted in the Git
common directory. Omit --jobs for the native scheduler's available parallelism.

Coverage has two independent axes. IMPLEMENTATION coverage (below)
chooses how many impls each case runs on; CASE coverage chooses how many
cases of each corpus run their impls at all. They compose.

Implementation coverage (required):
  SAMPLE_IMPL        For each case in an implementation matrix,
                     run exactly one applicable impl, picked at
                     random per run. Baseline local integration gate.
  FULL_IMPL_MATRIX   Run every applicable implementation. Very slow;
                     use when exhaustive backend coverage is required.
  <name>...          Restrict implementation-matrix checks to the
                     named impls, e.g. kio@js or kio@js kio@rust.
                     A comma-separated list is also accepted.

Case coverage (optional; each corpus has its own default):
  (default)          regular/direct Prime goldens: all
                     dynamic Prime: sampled (50)   emissions: all   poc: all
                     castles: sampled   contrib: sampled
                     The everyday local gate. Dynamic Prime is a separate
                     differential; its default does not narrow regular or
                     direct Prime goldens. Implementation selectors do not
                     override case coverage.
  --sample-cases     Sample every corpus at its own default cap. What
                     the GitHub gate passes: the whole corpus does not
                     fit in its time budget.
  --all-cases        Run every case of every corpus. The exhaustive
                     pass — use when a change could touch anything.
  --case-coverage=<scope>:<policy>
                     Repeatable registered root/verification-leaf override;
                     wins over --sample-cases / --all-cases. Policy is all,
                     sample, a positive per-bucket cap, or a registered set.
  --impl-verification=<scope>
                     Repeatable fixed-verifier augmentation for explicit
                     implementation lists. It changes no case selection.
  --case-seed=<S>    Pin the case draw so a sampled run reproduces
                     exactly. One seed covers every sampling corpus; it is
                     inert for a corpus running every case. Default: the
                     GitHub Actions run context when present, else UTC
                     epoch seconds. The effective seed is printed by each
                     sampling orchestrator.

  Case narrowing scopes the build+run tier only: a narrowed-out case still runs
  every \`# ROUTING: case-binary\` check its corpus wires, so that corpus's
  source invariants stay gated on every in-cohort case. A KNOWN_FAILING bug
  reproducer is never narrowed out, including by a named set.

  The generative orchestrator is not a corpus and takes no case flags;
  --gen-count / --gen-seed below are its equivalents.

Options:
  --gen-count=<N>    Override the number of programs the generative-tests
                     orchestrator generates. Useful for scoping a faster
                     local pass; forwarded to
                     ci/checks/orchestrators/generative-tests.sh, which
                     owns the default.
  --gen-seed=<N>     Pin the generative-tests orchestrator seed. Useful
                     for reproducing generated-case failures.
  --jobs=<N>         Override the shared scheduler slot count. Default:
                     the native scheduler's available parallelism.
  --compiler-jobs=<N>
                     Cap top-level Cargo invocations, compiler-producing Kio
                     commands, and actual native compiler commands across
                     worktrees. Omission uses paced, best-effort CPU/memory
                     feedback; a numeric value is a fixed cap.
  --keep-logs        Keep the auto-created per-task temp log directory
                     after the run. By default it is deleted after
                     pass, fail, or abort.
  --keep-logs=<DIR>  Write the per-task log directory at DIR and keep it.
                     DIR must not exist, or must be an empty directory.
  --no-live          Suppress the live progress stream (task starts,
                     completions, and per-failure diagnostics). Failures
                     still appear in the end-of-run report.
  -h, --help         Show this help and exit.
EOF
      exit 0
      ;;
    --sample-cases)
      CASES_MODE=sample
      ;;
    --all-cases)
      CASES_MODE=all
      ;;
    --goldens=*|--emissions=*|--poc=*|--castles=*|--contrib=*)
      old_flag=${1%%=*}; old_value=${1#*=}; old_scope=${old_flag#--}
      printf 'error: %s was removed; use --case-coverage=%s:%s\n' "$old_flag" "$old_scope" "$old_value" >&2
      exit 2
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
    --case-seed=*)
      CASE_SEED=${1#--case-seed=}
      ;;
    --case-seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
      CASE_SEED=$1
      ;;
    --gen-count=*)
      GEN_COUNT=${1#--gen-count=}
      ;;
    --gen-count)
      shift
      [ $# -gt 0 ] || { printf 'error: --gen-count requires a value\n' >&2; exit 2; }
      GEN_COUNT=$1
      ;;
    --gen-seed=*)
      GEN_SEED=${1#--gen-seed=}
      ;;
    --gen-seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --gen-seed requires a value\n' >&2; exit 2; }
      GEN_SEED=$1
      ;;
    --jobs=*)
      JOBS=${1#--jobs=}
      ;;
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
    --keep-logs)
      KEEP_LOGS=1
      ;;
    --keep-logs=*)
      KEEP_LOGS=1
      LOG_DIR=${1#--keep-logs=}
      [ -n "$LOG_DIR" ] || { printf 'error: --keep-logs= requires a non-empty directory\n' >&2; exit 2; }
      ;;
    --no-live)
      LIVE=0
      ;;
    --*) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    *)
      if [ -z "$IMPLS_SPEC" ]; then
        IMPLS_SPEC=$1
      else
        IMPLS_SPEC=$IMPLS_SPEC,$1
      fi
      ;;
  esac
  shift
done

case "${IMPLS_SPEC:-}" in
  "")
    printf 'error: ci/all.sh requires implementation coverage: SAMPLE_IMPL, FULL_IMPL_MATRIX, or one or more impl names\n' >&2
    exit 2
    ;;
  *,ANY,*|ANY,*|*,ANY|ANY)
    printf 'error: selector ANY was renamed to SAMPLE_IMPL\n' >&2
    exit 2
    ;;
  *,ALL,*|ALL,*|*,ALL|ALL)
    printf 'error: selector ALL was renamed to FULL_IMPL_MATRIX\n' >&2
    exit 2
    ;;
  SAMPLE_IMPL)
    IMPLS_MODE=sample
    ;;
  FULL_IMPL_MATRIX)
    IMPLS_MODE=full
    ;;
  ONLY:*)
    printf 'error: ONLY: selectors are no longer supported; pass explicit impl names directly\n' >&2
    exit 2
    ;;
  *)
    IMPLS_MODE=list
    IMPLS_ONLY=$IMPLS_SPEC
    ;;
esac

validate_debug_sample_impl_seed() {
  vdsis_value=$1
  case "$vdsis_value" in
    ''|*[!0-9]*|0?*) return 1 ;;
  esac
  [ "${#vdsis_value}" -le 10 ] || return 1
  if [ "${#vdsis_value}" -eq 10 ]; then
    LC_ALL=C awk -v value="$vdsis_value" \
      'BEGIN { exit !((value + 0) <= 4294967295) }' </dev/null || return 1
  fi
}

debug_sample_impl_seed_set=0
debug_sample_impl_seed=
if [ "${KIO_DEBUG_SAMPLE_IMPL_SEED+x}" = x ]; then
  validate_debug_sample_impl_seed "$KIO_DEBUG_SAMPLE_IMPL_SEED" || {
    printf 'error: KIO_DEBUG_SAMPLE_IMPL_SEED must be canonical decimal in 0..4294967295\n' >&2
    exit 2
  }
  [ "$IMPLS_MODE" = sample ] || {
    printf 'error: KIO_DEBUG_SAMPLE_IMPL_SEED requires SAMPLE_IMPL coverage\n' >&2
    exit 2
  }
  debug_sample_impl_seed_set=1
  debug_sample_impl_seed=$KIO_DEBUG_SAMPLE_IMPL_SEED
fi
unset KIO_DEBUG_SAMPLE_IMPL_SEED KIO_DEBUG_SAMPLE_IMPL_SCOPE

coverage_policy_validate_registry "$SCRIPT_DIR/checks/orchestrators"

# Validate and deduplicate requested fixed-verification leaves.
policy_tmp_parent=${TMPDIR:-${SCRIPT_DIR%/ci}/target}
mkdir -p "$policy_tmp_parent" || {
  printf 'error: cannot create coverage-policy scratch parent: %s\n' \
    "$policy_tmp_parent" >&2
  exit 2
}
policy_tmp=$(mktemp -d "$policy_tmp_parent/ci-all-coverage-policy.XXXXXX") ||
  exit 2
trap 'rm -rf "${policy_tmp:-}"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
verification_scopes_file=$policy_tmp/verification-scopes
verification_scopes_raw=$policy_tmp/verification-scopes.raw
printf '%s\n' "$IMPL_VERIFICATION_SCOPES" >"$verification_scopes_raw"
while IFS= read -r verification_scope; do
  [ -n "$verification_scope" ] || continue
  coverage_policy_validate_identifier "$verification_scope" ||
    coverage_policy_die "invalid implementation-verification scope: $verification_scope"
  verification_class=$(coverage_policy_classification_for_scope "$verification_scope" 2>/dev/null || :)
  [ -n "$verification_class" ] ||
    coverage_policy_die "unknown implementation-verification scope: $verification_scope"
  [ "$verification_class" = fixed-verification ] ||
    coverage_policy_die "--impl-verification=$verification_scope requires a fixed-verification scope"
  printf '%s\n' "$verification_scope"
done <"$verification_scopes_raw" >"$policy_tmp/verification-scopes.unsorted"
LC_ALL=C sort -u "$policy_tmp/verification-scopes.unsorted" >"$verification_scopes_file"

case_policies_file=$policy_tmp/case-policies.tsv
case_policies_raw=$policy_tmp/case-policies.raw
printf '%s\n' "$SCOPED_CASE_POLICIES" >"$case_policies_raw"
while IFS= read -r coverage_spec; do
  [ -n "$coverage_spec" ] || continue
  case "$coverage_spec" in *:*) ;; *) coverage_policy_die "invalid --case-coverage=$coverage_spec; expected <scope>:<policy>" ;; esac
  coverage_scope=${coverage_spec%%:*}; coverage_value=${coverage_spec#*:}
  coverage_policy_validate_identifier "$coverage_scope" || coverage_policy_die "invalid case-coverage scope: $coverage_scope"
  coverage_policy_validate_policy "$coverage_value" || coverage_policy_die "invalid case-coverage policy: $coverage_value"
  if ! awk -F '\t' -v scope="$coverage_scope" 'NR > 1 && ($2 == scope) { found=1 } END { exit !found }' "$COVERAGE_ORCHESTRATOR_REGISTRY" &&
     ! coverage_policy_owner_for_scope "$coverage_scope" >/dev/null 2>&1; then
    coverage_policy_die "unknown case-coverage scope: $coverage_scope"
  fi
  case "$coverage_value" in
    all|sample|*[!0-9]*)
      case "$coverage_value" in
        all|sample) ;;
        *) coverage_policy_named_set_registered "$coverage_scope" "$coverage_value" ||
             coverage_policy_die "unknown named case policy $coverage_value for scope $coverage_scope"
           coverage_policy_validate_named_set_manifest "$coverage_scope" "$coverage_value" ;;
      esac
      ;;
  esac
  printf '%s\t%s\n' "$coverage_scope" "$coverage_value"
done <"$case_policies_raw" >"$case_policies_file"

impl_list_for_owner() {
  ilfo_owner=$1
  ilfo_remaining=$IMPLS_ONLY
  ilfo_seen_fixed_scopes=,
  while :; do
    case $ilfo_remaining in
      *,*) ilfo_impl=${ilfo_remaining%%,*}; ilfo_remaining=${ilfo_remaining#*,} ;;
      *) ilfo_impl=$ilfo_remaining; ilfo_remaining= ;;
    esac
    if [ -n "$ilfo_impl" ]; then
      ilfo_scope=$(coverage_policy_verification_scope_for_impl "$ilfo_impl" 2>/dev/null || :)
      if [ -z "$ilfo_scope" ]; then
        printf '%s\n' "$ilfo_impl"
      elif [ "$(coverage_policy_owner_for_scope "$ilfo_scope")" = "$ilfo_owner" ]; then
        case $ilfo_seen_fixed_scopes in
          *",$ilfo_scope,"*) ;;
          *)
            printf '%s\n' "$ilfo_impl"
            ilfo_seen_fixed_scopes=$ilfo_seen_fixed_scopes$ilfo_scope,
            ;;
        esac
      fi
    fi
    [ -n "$ilfo_remaining" ] || break
  done
  while IFS= read -r ilfo_scope; do
    [ -n "$ilfo_scope" ] || continue
    [ "$(coverage_policy_owner_for_scope "$ilfo_scope")" = "$ilfo_owner" ] || continue
    case $ilfo_seen_fixed_scopes in
      *",$ilfo_scope,"*) ;;
      *)
        coverage_policy_fixed_impl_for_scope "$ilfo_scope"
        ilfo_seen_fixed_scopes=$ilfo_seen_fixed_scopes$ilfo_scope,
        ;;
    esac
  done <"$verification_scopes_file"
}

last_policy_for_scope() {
  awk -F '\t' -v scope="$1" \
    '$1 == scope { value=$2 } END { if (value != "") print value }' \
    "$case_policies_file"
}

if [ "$IMPLS_MODE" = list ]; then
  while IFS=$TAB read -r targeted_scope targeted_policy; do
    targeted_class=$(coverage_policy_classification_for_scope "$targeted_scope")
    [ "$targeted_class" = fixed-verification ] || continue
    targeted_owner=$(coverage_policy_owner_for_scope "$targeted_scope")
    targeted_fixed=$(coverage_policy_fixed_impl_for_scope "$targeted_scope")
    impl_list_for_owner "$targeted_owner" >"$policy_tmp/targeted-owner-impls"
    if ! awk -v impl="$targeted_fixed" '$0 == impl { found=1; exit }
        END { exit !found }' "$policy_tmp/targeted-owner-impls"; then
      printf 'error: --case-coverage=%s:%s targets inactive verification scope %s; add --impl-verification=%s or select %s\n' \
        "$targeted_scope" "$targeted_policy" "$targeted_scope" \
        "$targeted_scope" "$targeted_fixed" >&2
      exit 2
    fi
  done <"$case_policies_file"
fi

# One seed covers every sampling corpus, so a single --case-seed replays
# the whole run's draw. Resolved here rather than left to each
# orchestrator: five orchestrators starting in parallel would each fall
# back to their own epoch-seconds default and could land on different
# seconds, making a run reproducible only one corpus at a time. Under
# GitHub Actions the run context is stable across the five anyway; this
# just makes the local case behave the same.
if [ -z "$CASE_SEED" ]; then
  if [ -n "${GITHUB_RUN_ID:-}" ]; then
    CASE_SEED="${GITHUB_RUN_ID}:${GITHUB_RUN_ATTEMPT:-1}"
  else
    CASE_SEED=$(date -u +%s)
  fi
fi

verification_summary=$(awk 'NF { value = value sep $0; sep = "," }
  END { print value }' "$verification_scopes_file")
[ -n "$verification_summary" ] || verification_summary=none
case_policy_summary=$(awk -F '\t' 'NF {
    value = value sep $1 ":" $2
    sep = ","
  }
  END { print value }' "$case_policies_file")
[ -n "$case_policy_summary" ] || case_policy_summary=none

case "$COMPILER_JOBS" in
  adaptive)
    [ "$COMPILER_JOBS_EXPLICIT" -eq 0 ] || {
      printf 'error: --compiler-jobs must be a positive integer (got adaptive)\n' >&2
      exit 2
    }
    ;;
  ''|*[!0-9]*) printf 'error: --compiler-jobs must be a positive integer\n' >&2; exit 2 ;;
  *) [ "$COMPILER_JOBS" -ge 1 ] || { printf 'error: --compiler-jobs must be >= 1\n' >&2; exit 2; } ;;
esac

if [ "$COMPILER_JOBS" = adaptive ]; then
  unset KIO_CI_SCHEDULE_COMPILER_JOBS
else
  KIO_CI_SCHEDULE_COMPILER_JOBS=$COMPILER_JOBS
  export KIO_CI_SCHEDULE_COMPILER_JOBS
fi

# Configure explicitly named wrappers before the scheduler bootstrap. The
# scheduler is then available to isolate the early daemon probe from any
# inherited lease or Windows Job. Actual compiler commands recheck after their
# queue wait, so the early probe is not a persistent readiness claim.
# shellcheck disable=SC1091
. "$SCRIPT_DIR/infra/sccache.sh"
kio_configure_sccache_environment

# Resolve and build the scheduler's compiler-policy authority once before fan-out.
# Every task inherits the immutable absolute binary path, so no claimant
# hashes sources or invokes Cargo on the hot path.
KIO_CI_SCHEDULER_BIN=$(sh "$SCRIPT_DIR/schedule.sh" --prepare)
export KIO_CI_SCHEDULER_BIN
sh "$SCRIPT_DIR/schedule.sh" --self-test
sh "$SCRIPT_DIR/schedule.sh" --readiness -- sh

if [ -z "$JOBS" ]; then
  JOBS=$(sh "$SCRIPT_DIR/schedule.sh" --available-parallelism)
fi
case "$JOBS" in
  ''|*[!0-9]*) printf 'error: --jobs must be a positive integer\n' >&2; exit 2 ;;
  *) [ "$JOBS" -ge 1 ] || { printf 'error: --jobs must be >= 1\n' >&2; exit 2; } ;;
esac
KIO_CI_SCHEDULE_JOBS=$JOBS
export KIO_CI_SCHEDULE_JOBS

if [ "${KIO_CI_SCHEDULE:-}" != DISABLE ]; then
  KIO_CI_SCHEDULE_DIR=$(sh "$SCRIPT_DIR/schedule.sh" --state-dir)
  export KIO_CI_SCHEDULE_DIR
fi

# Collect every script under ci/<bucket>/<name>.sh into a list of
# (name, path) records. We background each invocation and wait on all
# at the end, tallying via per-script status files in a temp dir so
# the final summary distinguishes pass from fail.
if [ -n "$LOG_DIR" ]; then
  TMPDIR_LOGS=$LOG_DIR
  if [ -e "$TMPDIR_LOGS" ]; then
    [ -d "$TMPDIR_LOGS" ] || { printf 'error: --keep-logs= path exists but is not a directory: %s\n' "$TMPDIR_LOGS" >&2; exit 2; }
    if [ -n "$(find "$TMPDIR_LOGS" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
      printf 'error: --keep-logs= directory is not empty: %s\n' "$TMPDIR_LOGS" >&2
      exit 2
    fi
  else
    mkdir -p "$TMPDIR_LOGS"
  fi
else
  TMPDIR_LOGS=$(mktemp -d)
fi
TMPDIR_LOGS=$(CDPATH='' cd -- "$TMPDIR_LOGS" && pwd)
LOGS_KEEP=

# Guarantee exactly one grep-able terminal line — "$PROG: DONE <status>"
# — on every *catchable* exit, so a watcher can always tell "finished"
# (pass/fail) from "died mid-run" (aborted) instead of seeing silence
# when a signal or a `set -e` crash skips the normal summary below.
# emit_done is idempotent (first caller wins): the normal pass/fail path
# calls it, and the EXIT trap calls it with "aborted" if nothing did.
# CAVEAT: SIGKILL (kill -9) and the OOM-killer cannot be trapped, so they
# still emit no line — a watcher must fall back to a liveness/timeout
# check for those.
done_status=
emit_done() {
  if [ -n "$done_status" ]; then return 0; fi
  done_status=$1
  printf '%s: DONE %s\n' "$PROG" "$done_status"
}

# Clean finish (pass/fail/abort): the temp logs are scratch unless the
# caller explicitly requested retention. A named handler (not an inline
# trap string) keeps the exit-code capture visible to static analysis.
on_exit() {
  exit_st=$?
  rm -rf "${policy_tmp:-}"
  if [ -z "$done_status" ]; then
    # Ignore further catchable signals while we tear down, so a repeated
    # Ctrl-C (or a TERM storm) cannot interrupt the reap mid-pass and leave
    # half the worker groups alive. ci/all.sh is already exiting.
    trap '' INT TERM HUP
    # Abort path (a catchable signal or a `set -e` crash skipped the normal
    # summary). Reap every worker process group whose leader recorded its id
    # during the spawn phase, so the task scripts and their `run.sh` /
    # `kio build` / `kio-prime build` descendants die with us instead of
    # reparenting to init and racing a later run in this worktree. Each id
    # names a group distinct from this run's own group (the tasks ran under
    # `setsid`), so signalling the group never reaches ci/all.sh or its
    # caller. A second KILL pass covers any descendant that ignores TERM. The
    # normal pass/fail path never enters this branch, so a clean run is
    # unaffected.
    #
    # Negating zero targets the caller's group; negating one broadcasts.
    # Validate canonical decimal within the supported signed pid_t range
    # before the shell builtin can parse or narrow the operand.
    reap_groups() {
      reap_sig=$1
      for reap_f in "$TMPDIR_LOGS"/logs/*.pgid; do
        [ -f "$reap_f" ] || continue
        reap_pgid=$(cat "$reap_f" 2>/dev/null) || continue
        case "$reap_pgid" in ''|0*|1|*[!0-9]*) continue ;; esac
        [ "${#reap_pgid}" -le 10 ] || continue
        [ "$reap_pgid" -le 2147483647 ] 2>/dev/null || continue
        kill "-$reap_sig" "-$reap_pgid" 2>/dev/null || true
      done
    }
    if [ -n "${TMPDIR_LOGS:-}" ] && [ -d "$TMPDIR_LOGS/logs" ]; then
      reap_groups TERM
      sleep 0.2
      reap_groups KILL
    fi
    if [ -n "${progress_pid:-}" ]; then
      kill "$progress_pid" 2>/dev/null || true
    fi
    exec 3>&- 2>/dev/null || true
    emit_done "aborted (exit $exit_st)"
    if [ -n "${KEEP_LOGS:-}" ]; then
      LOGS_KEEP=1
      printf '%s: aborted; logs kept at %s/logs/\n' "$PROG" "$TMPDIR_LOGS" >&2
    else
      rm -rf "$TMPDIR_LOGS"
      printf '%s: aborted; temp logs deleted; rerun with --keep-logs or --keep-logs=<DIR> to keep logs\n' "$PROG" >&2
    fi
  else
    if [ -n "$LOGS_KEEP" ]; then
      :
    else
      rm -rf "$TMPDIR_LOGS"
    fi
  fi
}
trap on_exit EXIT
# Make catchable signals terminate (firing the EXIT trap) instead of
# resuming after the handler.
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

if [ "$LIVE" = 1 ]; then
  printf '%s: streaming live task progress and per-failure diagnostics; per-task logs go to %s/logs/\n' "$PROG" "$TMPDIR_LOGS" >&2
else
  printf '%s: --no-live: live progress suppressed; failures reported at end; per-task logs go to %s/logs/\n' "$PROG" "$TMPDIR_LOGS" >&2
fi
if [ -z "${KEEP_LOGS:-}" ]; then
  printf '%s: temp logs will be deleted after pass/fail/abort; use --keep-logs or --keep-logs=<DIR> to keep them\n' "$PROG" >&2
else
  printf '%s: logs will be kept after the run\n' "$PROG" >&2
fi
printf "%s: final line will be '%s: DONE pass', '%s: DONE fail', or '%s: DONE aborted ...'  (grep-able)\n" "$PROG" "$PROG" "$PROG" "$PROG" >&2
printf '%s: cargo env: RUSTC_WRAPPER=%s CARGO_INCREMENTAL=%s KIO_TEST_RUNNER_COMPILER_WRAPPER=%s KIO_CI_SERIALIZE_CARGO=%s\n' \
  "$PROG" "${RUSTC_WRAPPER:-}" "${CARGO_INCREMENTAL:-}" "${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}" "${KIO_CI_SERIALIZE_CARGO:-}" >&2
printf '%s: scheduler jobs: %s; compiler jobs: %s\n' \
  "$PROG" "$KIO_CI_SCHEDULE_JOBS" "$COMPILER_JOBS" >&2
printf '%s: case coverage: global=%s; scoped overrides=%s; fixed verifications=%s; case seed=%s\n' \
  "$PROG" "${CASES_MODE:-per-corpus-default}" "$case_policy_summary" \
  "$verification_summary" "$CASE_SEED" >&2

total_start=$(date +%s)

# Detect GNU time for per-task CPU accounting. Wall time is misleading on
# parallel tasks contending for CPU or memory; CPU time (user + sys) shows
# what each task actually did. Falls back to wall-only when
# /usr/bin/time isn't GNU time (notably macOS).
HAS_GNU_TIME=
if /usr/bin/time -f '' true >/dev/null 2>&1; then
  HAS_GNU_TIME=1
fi

# Spawn one subshell per script. Each writes its combined output to
# logs/<safe>.log and its exit code to logs/<safe>.exit. Output is
# buffered (no streaming) so the parallel logs don't interleave. A
# single progress reporter owns stdout during the run; the parent sends
# start records and task wrappers send one short completion record after
# their buffered log is closed.
#
# `set +e` in the subshell so the script's exit status is captured
# rather than aborting the subshell (which would leave .exit unwritten
# and confuse the tally loop's `cat "$exit_file"`).
mkdir -p "$TMPDIR_LOGS/logs"

progress_fifo="$TMPDIR_LOGS/progress.fifo"
mkfifo "$progress_fifo"
progress_pid=

# Stop the reporter on an explicit sentinel rather than FIFO EOF: a task's
# cargo run can spawn a long-lived daemon (e.g. the sccache server) that
# inherits fd 3 and outlives the run, so the write end never fully closes
# and EOF never arrives. Finalize writes this line once; the reporter consumes
# it without printing, so it can never leak into real output.
progress_eof='__ci-all-progress-eof__'

# Emit one failed task's diagnostics — failure-marker grep plus an 80-line
# log tail (and, for the generative task, its reproduction command). Shared
# by the live reporter (as each task fails) and the end-of-run report, so a
# failure renders identically in both. Defined before the reporter is
# spawned so the reporter subshell inherits it.
print_generative_repro_diagnostics() {
  log=$1
  printf '\n----- checks/orchestrators/generative-tests: reproduction -----\n' >&2
  metadata=$(grep -E '^kio-gen: (seed|count|prime-only) = ' "$log" 2>/dev/null || true)
  if [ -n "$metadata" ]; then
    printf '%s\n' "$metadata" >&2
  else
    printf '(kio-gen reproduction metadata not found)\n' >&2
  fi

  seed=$(sed -n 's/^kio-gen: seed = //p' "$log" | tail -n 1)
  count=$(sed -n 's/^kio-gen: count = //p' "$log" | tail -n 1)
  if [ -n "$seed" ] && [ -n "$count" ]; then
    printf 'reproduce batch via ci/all.sh: sh ci/all.sh SAMPLE_IMPL --gen-seed=%s --gen-count=%s\n' "$seed" "$count" >&2
    printf 'reproduce batch directly: sh ci/checks/orchestrators/generative-tests.sh --seed=%s --count=%s\n' "$seed" "$count" >&2
    printf 'rerun one failed case: sh ci/checks/orchestrators/generative-tests.sh --impls=<impl> --seed=%s --count=%s -- <case>\n' "$seed" "$count" >&2
    printf 'Use the impl and case from a FAIL [<impl>] line for the <impl> and <case> placeholders.\n' >&2
  else
    printf 'reproduction command unavailable: missing seed or count in log\n' >&2
  fi

  if grep -qE '^FAIL \[' "$log"; then
    printf 'failed case markers:\n' >&2
    grep -E '^FAIL \[' "$log" | sort -u | sed 's/^/  /' >&2
  fi
}

print_one_failed_task() {
  n=$1
  safe=$(printf '%s' "$n" | tr / _)
  log="$TMPDIR_LOGS/logs/$safe.log"
  printf '\n----- %s: failure markers -----\n' "$n" >&2
  if [ ! -r "$log" ]; then
    printf 'missing log: %s\n' "$log" >&2
    return 0
  fi
  if [ "$n" = checks/orchestrators/generative-tests ]; then
    print_generative_repro_diagnostics "$log"
  fi
  if ! grep -nE '^(FAIL \[|error:|  !! |failures:|---- .* stdout ----|thread .* panicked at)|^[[:space:]]+\[[^]]+/[^]]+\].*(mismatch|expected|actual|case has|contains no|diff)' "$log" >&2; then
    printf '(no standard failure markers found)\n' >&2
  fi
  printf '\n----- %s: log tail -----\n' "$n" >&2
  tail -n 80 "$log" >&2
}

progress_reporter() {
  while IFS= read -r line; do
    [ "$line" = "$progress_eof" ] && break
    [ "$LIVE" = 1 ] || continue
    printf '%s\n' "$line"
    # As each task fails, stream that task's failure diagnostics inline —
    # the same report the end-of-run summary emits — so the reader sees the
    # actual failure while the rest of the run continues, not just a marker.
    # The reporter owns stdout/stderr serially, so a full dump here can't
    # interleave with another task's output; the task wrapper writes this
    # marker only after its buffered log is closed, so the log is complete.
    case "$line" in
      "$PROG: TASK fail "*)
        rest=${line#"$PROG: TASK fail "}
        # `|| :` for the same reason the fd-3 marker writes carry it: a
        # non-zero return here must never abort the reporter's `set -e`
        # subshell and drop later live output.
        print_one_failed_task "${rest% (*}" || :
        ;;
    esac
  done <"$progress_fifo"
}
progress_reporter &
progress_pid=$!
exec 3>"$progress_fifo"

# Track scripts so we can iterate in deterministic order for the
# summary even though they finish in race-y order.
script_list="$TMPDIR_LOGS/scripts"
launched_script_list="$TMPDIR_LOGS/launched-scripts"
pid_list="$TMPDIR_LOGS/pids"
: >"$launched_script_list"
: >"$pid_list"
# Gating scripts live in exactly three places:
#   - ci/checks/orchestrators/*.sh — corpus + per-impl drivers
#   - ci/checks/repo-lint/*.sh     — repo-config lint
#   - ci/checks/hygiene/*.sh       — per-Rust-crate fmt + clippy + test
# Per-case scripts under ci/checks/per-case/ are invoked via
# `ci/run-tests.sh --check=`, not standalone, so they're excluded.
{
  find "$SCRIPT_DIR/checks/orchestrators" -mindepth 1 -maxdepth 1 -name '*.sh' -type f
  find "$SCRIPT_DIR/checks/repo-lint"     -mindepth 1 -maxdepth 1 -name '*.sh' -type f
  find "$SCRIPT_DIR/checks/hygiene"       -mindepth 1 -maxdepth 1 -name '*.sh' -type f
} | sort >"$script_list"

# Spawn phase.
while IFS= read -r script; do
  [ -n "$script" ] || continue
  rel=${script#"$SCRIPT_DIR"/}
  name=${rel%.sh}
  owner=${script##*/}
  owner_impls_pre=
  if [ "$IMPLS_MODE" = list ]; then
    case "$name" in
      checks/orchestrators/golden-tests|checks/orchestrators/emissions-tests|checks/orchestrators/generative-tests|checks/orchestrators/poc-tests|checks/orchestrators/castle-tests|checks/orchestrators/contrib-tests)
        impl_list_for_owner "$owner" >"$policy_tmp/$owner.impls"
        owner_impls_pre=$(awk 'NF { value = value sep $0; sep="," }
          END { print value }' "$policy_tmp/$owner.impls")
        # An explicit list containing only another owner's private fixed
        # verifier leaves this owner with no implementation work.
        [ -n "$owner_impls_pre" ] || continue
        ;;
    esac
  fi
  printf '%s\n' "$script" >>"$launched_script_list"
  safe=$(printf '%s' "$name" | tr / _)
  log="$TMPDIR_LOGS/logs/$safe.log"
  exit_file="$TMPDIR_LOGS/logs/$safe.exit"
  time_file="$TMPDIR_LOGS/logs/$safe.time"
  cpu_file="$TMPDIR_LOGS/logs/$safe.cpu"
  : >"$cpu_file"
  # The task's setsid worker records its own process-group id here so the
  # abort path can reap the worker subtree (see the spawn block below and the
  # reap loop in on_exit).
  pgid_file="$TMPDIR_LOGS/logs/$safe.pgid"
  printf '%s: TASK start %s\n' "$PROG" "$name" >&3 || :
  (
    set +e
    if [ "$debug_sample_impl_seed_set" -eq 1 ]; then
      case "$name" in
        checks/orchestrators/golden-tests|checks/orchestrators/emissions-tests|checks/orchestrators/generative-tests|checks/orchestrators/poc-tests|checks/orchestrators/castle-tests|checks/orchestrators/contrib-tests)
          KIO_DEBUG_SAMPLE_IMPL_SEED=$debug_sample_impl_seed
          KIO_DEBUG_SAMPLE_IMPL_SCOPE=$name
          export KIO_DEBUG_SAMPLE_IMPL_SEED KIO_DEBUG_SAMPLE_IMPL_SCOPE
          ;;
      esac
    fi
    set --
    case "$name" in
      checks/orchestrators/golden-tests|checks/orchestrators/emissions-tests|checks/orchestrators/generative-tests|checks/orchestrators/poc-tests|checks/orchestrators/castle-tests|checks/orchestrators/contrib-tests)
        case "$IMPLS_MODE" in
          sample) set -- "$@" --impls=SAMPLE_IMPL ;;
          list)
            owner_impls=$owner_impls_pre
            set -- "$@" "--impls=$owner_impls"
            ;;
          full) set -- "$@" --impls=FULL_IMPL_MATRIX ;;
        esac
        if [ "$name" = checks/orchestrators/generative-tests ] && [ -n "${GEN_COUNT:-}" ]; then
          set -- "$@" "--count=$GEN_COUNT"
        fi
        if [ "$name" = checks/orchestrators/generative-tests ] && [ -n "${GEN_SEED:-}" ]; then
          set -- "$@" "--seed=$GEN_SEED"
        fi
        # Case coverage. The generative orchestrator is not a corpus — it
        # synthesizes its programs, and --gen-count is its size knob — so
        # it takes no case flags. For the five corpora, a per-corpus
        # override wins over the global --sample-cases / --all-cases; with
        # neither, pass no flag and let the orchestrator apply its own
        # default (regular/direct Prime goldens, emissions, and poc whole;
        # dynamic Prime, castles, and contrib sampled).
        case "$name" in
          checks/orchestrators/generative-tests) ;;
          *)
            root_scope=$(coverage_policy_root_for_owner "$owner")
            corpus_cases=$(last_policy_for_scope "$root_scope")
            case "$corpus_cases" in
              '')
                case "$CASES_MODE" in
                  sample) set -- "$@" --sample-cases ;;
                  all) set -- "$@" --all-cases ;;
                esac
                ;;
              all) set -- "$@" --all-cases ;;
              sample) set -- "$@" --sample-cases ;;
              *) set -- "$@" "--case-count=$corpus_cases" ;;
            esac
            # Harmless when the corpus ends up running whole: the
            # orchestrator drops the seed unless it actually samples.
            set -- "$@" "--case-seed=$CASE_SEED"
            while IFS=$TAB read -r leaf_scope _leaf_owner _parent _class _fixed _toolchain; do
              [ "$leaf_scope" = scope ] && continue
              [ "$_leaf_owner" = "$owner" ] || continue
              leaf_policy=$(last_policy_for_scope "$leaf_scope")
              [ -z "$leaf_policy" ] || set -- "$@" "--case-coverage=$leaf_scope:$leaf_policy"
            done <"$COVERAGE_VERIFICATION_SCOPES"
            ;;
        esac
        ;;
      *) ;;
    esac
    start=$(date +%s)
    # Run the script in its own process group so the abort path can reap the
    # script and its `run.sh` / `kio build` / `kio-prime build` descendants as
    # a unit instead of letting them orphan to init and race a later run in
    # this worktree. `setsid` runs the work as a fresh session+group leader.
    # We can't read that leader's pgid reliably from here (a foreground
    # `setsid` forks, and whether `$!` of a backgrounded one is the leader is
    # racy), so the leader records its OWN pgid to "$pgid_file" before exec'ing
    # the work. The group is distinct from this run's own group, so reaping it
    # never signals ci/all.sh or its caller. The work runs in the foreground,
    # so this subshell's status/timing capture below is unchanged from a plain
    # synchronous invocation — the normal pass/fail path is unaffected.
    #
    # The inner script is single-quoted on purpose — `$$`, `$1`, `$@` must
    # expand in the setsid worker, not here (shellcheck SC2016 flags the intent,
    # hence the disable). Its positional parameters are the pgid file, then (for
    # the timed branch) the cpu file, then the script and the script's own args.
    if [ -n "$HAS_GNU_TIME" ]; then
      # shellcheck disable=SC2016
      KIO_CI_PROGRESS_FD=3 KIO_CI_TASK_NAME=$name \
        setsid -w sh -c '
          pgid=$(sed -n "s/^.*) //p" "/proc/$$/stat" | cut -d" " -f3)
          printf "%s\n" "$pgid" >"$1"
          cpu_file=$2; script=$3; shift 3
          exec /usr/bin/time -o "$cpu_file" -f "KIO_CI_CPU %U %S" sh "$script" "$@"
        ' sh "$pgid_file" "$cpu_file" "$script" "$@" >"$log" 2>&1
    else
      # shellcheck disable=SC2016
      KIO_CI_PROGRESS_FD=3 KIO_CI_TASK_NAME=$name \
        setsid -w sh -c '
          pgid=$(sed -n "s/^.*) //p" "/proc/$$/stat" | cut -d" " -f3)
          printf "%s\n" "$pgid" >"$1"
          script=$2; shift 2
          exec sh "$script" "$@"
        ' sh "$pgid_file" "$script" "$@" >"$log" 2>&1
    fi
    status=$?
    end=$(date +%s)
    cpu='-'
    if [ -s "$cpu_file" ]; then
      cpu=$(sh "$CPU_SUM" "$cpu_file" 2>/dev/null) || cpu='-'
      [ -z "$cpu" ] && cpu='-'
    fi
    # Fields: start_offset end_offset wall cpu (offsets relative to total_start).
    printf '%d %d %d %s\n' \
      "$((start - total_start))" "$((end - total_start))" "$((end - start))" "$cpu" \
      >"$time_file.tmp"
    mv "$time_file.tmp" "$time_file"
    echo "$status" >"$exit_file.tmp"
    mv "$exit_file.tmp" "$exit_file"
    if [ "$status" -eq 0 ]; then
      result=pass
    else
      result=fail
    fi
    printf '%s: TASK %s %s (%ss wall, %ss cpu)\n' \
      "$PROG" "$result" "$name" "$((end - start))" "$cpu" >&3 || :
  ) &
  echo "$!" >>"$pid_list"
done <"$script_list"

# Wait phase — every task wrapper, but not the progress reporter. Close
# the progress pipe only after task wrappers finish, then wait for the
# reporter so completion lines cannot interleave with the final log dump.
while IFS= read -r pid; do
  [ -n "$pid" ] || continue
  wait "$pid" || true
done <"$pid_list"
printf '%s\n' "$progress_eof" >&3 2>/dev/null || :
exec 3>&-
wait "$progress_pid" || true
progress_pid=

# Tally + report. Print each script's log under a banner, then
# summarize pass/fail.
successes=""
failures=""
while IFS= read -r script; do
  [ -n "$script" ] || continue
  rel=${script#"$SCRIPT_DIR"/}
  name=${rel%.sh}
  safe=$(printf '%s' "$name" | tr / _)
  log="$TMPDIR_LOGS/logs/$safe.log"
  exit_file="$TMPDIR_LOGS/logs/$safe.exit"
  time_file="$TMPDIR_LOGS/logs/$safe.time"
  start_off='?'; end_off='?'; wall='?'; cpu='?'
  if [ -r "$time_file" ]; then
    read -r start_off end_off wall cpu <"$time_file" || true
  fi
  printf '\n========== %s (%ss wall, %ss cpu, %s→%s) ==========\n' \
    "$name" "$wall" "$cpu" "$start_off" "$end_off"
  cat "$log"
  exit_status=missing
  if [ -r "$exit_file" ]; then
    exit_status=$(cat "$exit_file")
  fi
  if [ "$exit_status" = 0 ]; then
    successes="$successes $name"
  else
    failures="$failures $name"
  fi
done <"$launched_script_list"

total_end=$(date +%s)

# Emit "<wall>\t<name>\t<start>\t<end>\t<cpu>" lines for each task in
# $1, sort by descending wall, format each row. $2 = "1" to send to
# stderr (failures), empty to send to stdout (successes).
print_group() {
  group_names=$1
  to_stderr=$2
  for n in $group_names; do
    safe=$(printf '%s' "$n" | tr / _)
    s=0; e=0; w=0; c='-'
    if [ -r "$TMPDIR_LOGS/logs/$safe.time" ]; then
      read -r s e w c <"$TMPDIR_LOGS/logs/$safe.time" || true
    fi
    printf '%d\t%s\t%s\t%s\t%s\n' "$w" "$n" "$s" "$e" "$c"
  done | sort -k1,1nr | while IFS="$(printf '\t')" read -r w n s e c; do
    if [ -n "$to_stderr" ]; then
      printf '  %4ss wall  %6ss cpu  %3s→%-3s  %s\n' "$w" "$c" "$s" "$e" "$n" >&2
    else
      printf '  %4ss wall  %6ss cpu  %3s→%-3s  %s\n' "$w" "$c" "$s" "$e" "$n"
    fi
  done
}

print_failed_task_diagnostics() {
  group_names=$1
  [ -n "$group_names" ] || return 0
  printf '\n========== failed task diagnostics ==========\n' >&2
  for n in $group_names; do
    print_one_failed_task "$n"
  done
}

printf '\n========== summary ==========\n'
printf 'total wall: %ds' "$((total_end - total_start))"
if [ -n "$HAS_GNU_TIME" ]; then
  # Sum user+sys across all tasks; ratio vs wall is the parallelism factor.
  total_cpu=$(sh "$CPU_SUM" "$TMPDIR_LOGS"/logs/*.cpu 2>/dev/null) || total_cpu='-'
  [ -z "$total_cpu" ] && total_cpu='-'
  printf '   total cpu: %ss' "$total_cpu"
fi
printf '\n'
if [ -n "$successes" ]; then
  printf 'PASSED (sorted by wall, descending):\n'
  print_group "$successes" ''
fi
if [ -n "$failures" ]; then
  printf 'FAILED (sorted by wall, descending):\n' >&2
  print_group "$failures" 1
  print_failed_task_diagnostics "$failures"
fi

# Grep-able end-of-run marker on stdout. The startup notice tells the
# reader to look for this line; keep the format stable.
n_pass=$(printf '%s' "$successes" | wc -w | tr -d ' ')
n_fail=$(printf '%s' "$failures" | wc -w | tr -d ' ')
total_wall=$((total_end - total_start))
if [ "$n_fail" -gt 0 ]; then
  if [ -n "${KEEP_LOGS:-}" ]; then
    LOGS_KEEP=1
    printf '%s: logs kept at %s/logs/\n' "$PROG" "$TMPDIR_LOGS"
  else
    printf '%s: logs deleted; rerun with --keep-logs or --keep-logs=<DIR> to inspect full buffered logs after failure\n' "$PROG" >&2
  fi
  emit_done "fail (${total_wall}s wall, ${n_pass}/$((n_pass + n_fail)) passed, ${n_fail} failed)"
  exit 1
fi
if [ -n "${KEEP_LOGS:-}" ]; then
  LOGS_KEEP=1
  printf '%s: logs kept at %s/logs/\n' "$PROG" "$TMPDIR_LOGS"
fi
emit_done "pass (${total_wall}s wall, ${n_pass}/$((n_pass + n_fail)) passed)"
