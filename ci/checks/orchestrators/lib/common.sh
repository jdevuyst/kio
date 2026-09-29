# shellcheck shell=sh
#
# Shared helpers for the ci/checks/orchestrators/*.sh corpus
# orchestrators. This file is SOURCED, never executed: each orchestrator
# computes SCRIPT_DIR from its own $0 and does
#   . "$SCRIPT_DIR/lib/common.sh"
# so the helpers resolve no matter what the caller's cwd is (the macOS /
# Windows portability jobs run the orchestrators directly from an
# arbitrary directory, not through ci/all.sh). It defines no gating job:
# ci/all.sh's orchestrator discovery is non-recursive (find -maxdepth 1),
# so lib/ is never auto-run. See ai/topics/repo-layout.md.
#
# The impl-list helpers (filter_impls, validate_impls_against_list) read
# $TAB and $ONLY from the sourcing orchestrator's scope; apply_impls_spec
# sets that orchestrator's $ONLY and $SAMPLE_IMPL. Every orchestrator
# that uses these initializes TAB, ONLY, and SAMPLE_IMPL before calling
# them.
#
# NB: multi-line strings are passed to awk via env vars and read with
# ENVIRON[] rather than awk's `-v key=value`. BSD awk on macOS rejects
# newlines in -v values with "newline in string …"; ENVIRON is the
# POSIX-portable workaround. Applies to every awk call here.
#
# POSIX sh only.

# Create one ignored, worktree-local temporary root for a corpus
# orchestrator and route every child process's temporary files beneath it.
# The sourcing orchestrator owns the EXIT trap that removes
# $ORCHESTRATOR_TMP. Keeping worker scratch and private helper snapshots in
# this lifecycle avoids system-temp quotas and lets workers use an immutable
# executable rather than a mutable Cargo output.
init_orchestrator_tmp() {
  iot_name=$1
  case "$iot_name" in
    ''|*[!A-Za-z0-9._-]*)
      printf 'error: invalid orchestrator temp name: %s\n' "$iot_name" >&2
      exit 2
      ;;
  esac
  iot_parent="$REPO_ROOT/target"
  mkdir -p "$iot_parent"
  ORCHESTRATOR_TMP=$(mktemp -d "$iot_parent/$iot_name.XXXXXX")
  TMPDIR="$ORCHESTRATOR_TMP/tmp"
  if ! mkdir "$TMPDIR"; then
    rm -rf "$ORCHESTRATOR_TMP"
    printf 'error: cannot create orchestrator TMPDIR: %s\n' "$TMPDIR" >&2
    exit 1
  fi
  export ORCHESTRATOR_TMP TMPDIR
}

# Build one Cargo-backed corpus tool and copy an immutable snapshot into the
# calling orchestrator's private lifecycle. Normal scheduled calls converge on
# a dedicated target below the owning workspace's existing target/ cache. The
# capacity-one cargo resource covers both Cargo and the copy, so no caller can
# copy while another mutates that target; ci/cargo.sh then acquires compiler in
# canonical cargo -> compiler order. Each key names one fixed workspace and
# build-argument tuple; callers may copy any binary that tuple builds.
# An explicit scheduler bypass retains the former invocation-private target
# because the bypass provides no cargo lease.
#
# Arguments: key, absolute Cargo workspace, built binary name, destination,
# then Cargo build arguments after the helper-owned
# `build --target-dir ...` prefix.
build_corpus_tool_binary() {
  bctb_key=$1
  bctb_workspace=$2
  bctb_binary=$3
  bctb_destination=$4
  shift 4

  case "$bctb_key" in
    ''|.|..|*[!A-Za-z0-9._-]*)
      printf 'error: invalid corpus tool key: %s\n' "$bctb_key" >&2
      exit 2
      ;;
  esac
  case "$bctb_workspace" in
    /*|[A-Za-z]:[\\/]*) ;;
    *)
      printf 'error: corpus tool workspace must be absolute: %s\n' \
        "$bctb_workspace" >&2
      exit 2
      ;;
  esac

  if [ "${KIO_CI_SCHEDULE:-}" = DISABLE ]; then
    bctb_target=$ORCHESTRATOR_TMP/$bctb_key-target
    (
      cd "$bctb_workspace" || exit 2
      sh "$REPO_ROOT/ci/cargo.sh" build --target-dir "$bctb_target" "$@"
    )
    cp "$bctb_target/debug/$bctb_binary" "$bctb_destination"
    return
  fi

  bctb_target=$bctb_workspace/target/kio-corpus-tools/$bctb_key
  (
    cd "$bctb_workspace" || exit 2
    sh "$REPO_ROOT/ci/schedule.sh" --resource cargo -- sh -eu -c '
      repo_root=$1
      target=$2
      binary=$3
      destination=$4
      shift 4
      sh "$repo_root/ci/cargo.sh" build --target-dir "$target" "$@"
      cp "$target/debug/$binary" "$destination"
    ' sh "$REPO_ROOT" "$bctb_target" "$bctb_binary" \
      "$bctb_destination" "$@"
  )
}

# Default test-runner artifact-cache base for one suite, as an absolute
# path. Machine-stable and shared, so sibling worktrees reuse one cache:
# the cache key is path-normalized and the cached artifacts are
# path-neutral, so a hit from worktree A serves worktree B. Mirrors how
# sccache defaults to ~/.cache/sccache — $XDG_CACHE_HOME wins when set,
# else ~/.cache. This is the test-runner ARTIFACT cache only
# (KIO_TEST_RUNNER_BUILD_CACHE_DIR); the kio BUILD cache under
# out/.kio-cache/ stays worktree-local. A hermetic CI job that wants a
# worktree-local base appends `-- --cache-base=<dir>` (run-tests.sh
# takes the last --cache-base). See ai/topics/local-tools.md § Compiler
# cache.
shared_cache_base() {
  scb_suite=$1
  if [ -n "${XDG_CACHE_HOME:-}" ]; then
    printf '%s/kio/%s' "$XDG_CACHE_HOME" "$scb_suite"
  else
    printf '%s/.cache/kio/%s' "$HOME" "$scb_suite"
  fi
}

# Default per-impl size cap for the shared artifact cache (size-LRU,
# sccache-shaped — no max-age sweep). The cap is per impl-target: each
# runner prunes only its own $CACHE_BASE/<impl-target>/ subtree on
# write, so this bounds one target's corpus, sized to ~2 generations of
# the largest target's golden corpus with headroom so one `ci/all.sh`
# run never evicts its own early entries mid-run. An external
# KIO_TEST_RUNNER_BUILD_CACHE_SIZE wins (`:-`), e.g. to tighten the cap
# under disk pressure. See ai/topics/local-tools.md § Compiler cache.
set_default_build_cache_size() {
  KIO_TEST_RUNNER_BUILD_CACHE_SIZE="${KIO_TEST_RUNNER_BUILD_CACHE_SIZE:-8G}"
  export KIO_TEST_RUNNER_BUILD_CACHE_SIZE
}

# Validate and publish the compiler-process capacity before an orchestrator
# starts any Cargo prebuild. An omitted orchestrator flag inherits an outer
# gate's setting, then falls back to the repository default.
resolve_compiler_jobs() {
  rcj_value=$COMPILER_JOBS
  case "$rcj_value" in
    adaptive)
      [ "${COMPILER_JOBS_EXPLICIT:-0}" -eq 0 ] || {
        printf 'error: --compiler-jobs must be a positive integer (got adaptive)\n' >&2
        exit 2
      }
      ;;
    ''|*[!0-9]*)
      printf 'error: --compiler-jobs must be a positive integer (got %s)\n' \
        "$rcj_value" >&2
      exit 2
      ;;
    *)
      [ "$rcj_value" -ge 1 ] || {
        printf 'error: --compiler-jobs must be >= 1 (got %s)\n' \
          "$rcj_value" >&2
        exit 2
      }
      ;;
  esac
  COMPILER_JOBS=$rcj_value
  export COMPILER_JOBS
  if [ "$rcj_value" = adaptive ]; then
    unset KIO_CI_SCHEDULE_COMPILER_JOBS
  else
    KIO_CI_SCHEDULE_COMPILER_JOBS=$rcj_value
    export KIO_CI_SCHEDULE_COMPILER_JOBS
  fi

  # Standalone orchestrators prebuild runners before ci/run-tests.sh. Configure
  # wrappers, prepare the native scheduler, then isolate the early readiness
  # probe from inherited leases and Windows Jobs.
  if ! command -v kio_ensure_sccache_ready >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    . "$REPO_ROOT/ci/infra/sccache.sh"
  fi
  kio_configure_sccache_environment

  KIO_CI_SCHEDULER_BIN=$(sh "$REPO_ROOT/ci/schedule.sh" --prepare) || exit $?
  export KIO_CI_SCHEDULER_BIN
  sh "$REPO_ROOT/ci/schedule.sh" --readiness -- sh || exit $?
}

# Interpret an --impls=<spec> value: FULL_IMPL_MATRIX, SAMPLE_IMPL, or a
# comma-separated impl list. Sets the sourcing orchestrator's $ONLY and
# $SAMPLE_IMPL; rejects the renamed ANY / ALL / ONLY: selectors.
# SAMPLE_IMPL is read by the caller, not here — hence SC2034 is silenced.
# shellcheck disable=SC2034
apply_impls_spec() {
  spec=$1
  [ -n "$spec" ] || { printf 'error: --impls requires FULL_IMPL_MATRIX, SAMPLE_IMPL, or a comma-separated impl list\n' >&2; exit 2; }
  case "$spec" in
    FULL_IMPL_MATRIX) ONLY=; SAMPLE_IMPL=0 ;;
    SAMPLE_IMPL) ONLY=; SAMPLE_IMPL=1 ;;
    *,ANY,*|ANY,*|*,ANY|ANY)
      printf 'error: selector ANY was renamed to SAMPLE_IMPL\n' >&2
      exit 2
      ;;
    *,ALL,*|ALL,*|*,ALL|ALL)
      printf 'error: selector ALL was renamed to FULL_IMPL_MATRIX\n' >&2
      exit 2
      ;;
    ONLY:*)
      printf 'error: ONLY: selectors are no longer supported; pass explicit impl names directly\n' >&2
      exit 2
      ;;
    *)
      ONLY=$spec
      SAMPLE_IMPL=0
      ;;
  esac
}

# Filter an impl list (multi-line, tab-separated records) by the
# comma-separated $ONLY. With $ONLY empty, echo every non-empty record;
# otherwise echo only records whose name (field 1) is in $ONLY. Stdout:
# the filtered records.
filter_impls() {
  IMPL_LIST=$1 IMPL_ONLY=$ONLY awk -F"$TAB" '
    BEGIN {
      list = ENVIRON["IMPL_LIST"]
      only = ENVIRON["IMPL_ONLY"]
      n = split(list, lines, "\n")
      if (only == "") {
        for (i = 1; i <= n; i++) if (lines[i] != "") print lines[i]
        exit
      }
      m = split(only, want, ",")
      for (i = 1; i <= m; i++) want_set[want[i]] = 1
      for (i = 1; i <= n; i++) {
        if (lines[i] == "") continue
        split(lines[i], rec, "\t")
        if (rec[1] in want_set) print lines[i]
      }
    }
  '
}

# Validate the comma-separated $ONLY against a single impl list before
# any filtering, so a typo in one bucket does not silently fall through.
# On an unknown name, print an error naming the known impls and exit 2.
# A no-op when $ONLY is empty. Harnesses with several impl lists (e.g.
# golden-tests.sh) keep a union validator local instead.
validate_impls_against_list() {
  vial_list=$1
  [ -n "$ONLY" ] || return 0
  vial_unknown=$(IMPL_LIST=$vial_list IMPL_ONLY=$ONLY awk -F"$TAB" '
    BEGIN {
      list = ENVIRON["IMPL_LIST"]
      only = ENVIRON["IMPL_ONLY"]
      n = split(list, lines, "\n")
      for (i = 1; i <= n; i++) {
        if (lines[i] == "") continue
        split(lines[i], rec, "\t")
        known[rec[1]] = 1
      }
      missing = ""
      m = split(only, want, ",")
      for (i = 1; i <= m; i++) {
        if (want[i] == "") continue
        if (!(want[i] in known)) missing = missing " " want[i]
      }
      print missing
    }
  ')
  if [ -n "$vial_unknown" ]; then
    vial_regular=$(printf '%s\n' "$vial_list" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
    printf 'error: --impls: unknown impl(s):%s (known: %s)\n' \
      "$vial_unknown" "$vial_regular" >&2
    exit 2
  fi
}

# True when the file at $1 contains exactly the exit code 0, ignoring
# surrounding whitespace. The corpus contract validators call it to
# assert a success-only case's expected.exit is 0.
expected_exit_is_zero() {
  path=$1
  content=$(tr -d '[:space:]' <"$path")
  [ "$content" = 0 ]
}

# ---------------------------------------------------------------------
# Case sampling — how many CASES of a corpus run their impls this pass.
#
# Vocabulary: `--impls` / SAMPLE_IMPL / FULL_IMPL_MATRIX select
# IMPLEMENTATIONS; every flag naming `case` selects CASES. The two
# compose and are never spelled as a bare "sample", so a reader of any
# invocation can tell which axis is being cut. See TESTING.md
# § Local iteration.
#
# Each corpus orchestrator initializes CASE_MODE to its own default —
# `all` for regular/direct Prime goldens, emissions, and poc
# and `sample` for the ones it does not (castles, contrib) — plus
# DEFAULT_CASE_COUNT, the per-bucket cap its `sample` mode uses. The golden
# owner additionally resolves its dynamic verifier's leaf budget. These
# helpers implement the four flags every corpus orchestrator
# accepts:
#   --all-cases       every case runs its impls
#   --sample-cases    sample at DEFAULT_CASE_COUNT
#   --case-count=<N>  sample at N per top-level bucket
#   --case-seed=<S>   pin the draw for reproduction
#
# Sampling scopes the impl tier only: run-tests.sh still runs every
# `# ROUTING: case-binary` check on a sampled-out case. See its
# --sample-cases block for why that split is load-bearing.
#
# CASE_MODE / CASE_COUNT / CASE_SEED are read by case_sampling_args and
# written by the setters, both across the source boundary — hence SC2034.
# shellcheck disable=SC2034

# --all-cases
apply_all_cases() {
  CASE_MODE=all
}

# --sample-cases: sample at the corpus's own default per-bucket cap.
apply_sample_cases() {
  CASE_MODE=sample
  CASE_COUNT=$DEFAULT_CASE_COUNT
}

# --case-count=<N>: sample at N per top-level bucket. A cap of 0 would
# build and run nothing while still exiting 0, so the floor is 1.
apply_case_count() {
  acc_count=$1
  case "$acc_count" in
    ''|*[!0-9]*)
      printf 'error: --case-count must be a positive integer (got %s)\n' "$acc_count" >&2
      exit 2
      ;;
    0)
      printf 'error: --case-count=0 would build and run no case at all; use a positive integer, or --all-cases\n' >&2
      exit 2
      ;;
  esac
  CASE_MODE=sample
  CASE_COUNT=$acc_count
}

# --case-seed=<S>
apply_case_seed() {
  acs_seed=$1
  [ -n "$acs_seed" ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
  CASE_SEED=$acs_seed
}

# Resolve the effective case seed once, in the orchestrator's own shell,
# before any run-tests invocation. An orchestrator can invoke run-tests
# more than once (golden-tests.sh runs a regular, a --prime-only, and a
# --dyn-load-prime-only pass); letting each fall back to run-tests' own
# epoch-seconds default would let them land on different seconds, so a
# single --case-seed would no longer replay the orchestrator's whole draw.
# A no-op when the corpus is not sampling, or when the caller pinned a seed.
resolve_case_seed() {
  [ "$CASE_MODE" = sample ] || return 0
  [ -z "${CASE_SEED:-}" ] || return 0
  if [ -n "${GITHUB_RUN_ID:-}" ]; then
    CASE_SEED="${GITHUB_RUN_ID}:${GITHUB_RUN_ATTEMPT:-1}"
  else
    CASE_SEED=$(date -u +%s)
  fi
}

# Render the resolved case-sampling state as ci/run-tests.sh flags, one
# per line (empty output under CASE_MODE=all — run-tests defaults to
# every case). The caller appends the lines to its run-tests argv.
case_sampling_args() {
  [ "$CASE_MODE" = sample ] || return 0
  printf -- '--sample-cases=%s\n' "$CASE_COUNT"
  if [ -n "${CASE_SEED:-}" ]; then
    printf -- '--case-seed=%s\n' "$CASE_SEED"
  fi
}
