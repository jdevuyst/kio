#!/bin/sh
# shellcheck disable=SC2034 # Capacity-explicitness is read by sourced common.sh.
#
# Build the implementations + verifier and run the contributed
# test-case corpus through `ci/run-tests.sh`, with per-case checks
# (kio fmt idempotence, IS_KIO_PRIME biconditional, Kio' round-trip,
# TypeScript-strict typecheck, rlib-cache warm-hit) wired in via
# run-tests' `--check=` pipeline.
#
# Contrib cases are community-contributed Kio — the one lane where
# external pull requests are accepted; see CONTRIBUTING.md § Contribute
# an example program or library and the case contract in
# `test-data/contrib/README.md`. Each case is small and self-contained,
# in a flat directory named `<github-username>-<issue-number>`, with
# exactly one execution file selecting its shape:
#   - `run.args`       — an example program (standard build-then-run).
#   - `run.test-only`  — a library (kio test + kio build, no runner;
#                        requires an equiv law).
#   - `run.sh`         — a custom script; accepted only because a
#                        maintainer reviews it before running it in CI
#                        (see contrib-run.yml).
# A bug reproducer carries a `KNOWN_FAILING` marker (run-tests.sh treats
# it as an expected failure: the gate stays green, the run warns, and a
# marker that starts passing fails so it's cleaned up). `expected.exit`
# is always `0` — the desired successful outcome.
#
# Case sampling. This corpus SAMPLES by default, like castles. It is the
# one lane that grows from outside the repo — an unbounded number of
# arbitrarily large contributions — and it is not the regression net for
# anything: a contrib case that exposes a bug is answered by a focused
# golden under `test-data/goldens/` (CONTRIBUTING.md § Contribute an
# example program or library), and that golden is what guards the fix. So
# capping the contrib build+run tier loses no coverage that isn't held
# elsewhere. `--all-cases` runs the whole corpus. A sampled-out case still
# runs every `# ROUTING: case-binary` check this orchestrator wires, and a
# KNOWN_FAILING bug reproducer is never sampled out — its gate is that it
# fails once its bug is fixed, and that gate lives in the impl tier.
#
# The corpus ships a maintainer-owned `example-0` case so the harness
# stays exercised even before any real contribution lands; an empty
# corpus (example removed) still exits cleanly before the build phase.
#
# Single impl bucket: regular Kio compilers run against every case.
# There is no separate `kio-prime@js` source-execution bucket: contrib
# cases are natural surface packages, so a Kio'-only compiler has
# nothing dedicated to run here. The per-case prime-marker /
# kio-prime-roundtrip checks still assert the source-level Kio'
# biconditional and generated-artifact parity where their contracts
# fit.
#
# Usage:
#   sh ci/checks/orchestrators/contrib-tests.sh \
#     [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] \
#     [--compiler-jobs=<N>] \
#     [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] \
#     [-- <name>...]
#
# --impls=FULL_IMPL_MATRIX
#                  run all configured impls (default).
# --impls=SAMPLE_IMPL
#                  for each case, run exactly one applicable impl,
#                  picked at random per run.
# --impls=<list>   restrict run-tests to the named comma-separated impls.
# --jobs=<N>       passed through to run-tests as --jobs; default is
#                  run-tests' native available-parallelism default.
# --compiler-jobs=<N>
#                  cap top-level Cargo invocations, compiler-producing Kio
#                  commands, and runner compiler commands; omission uses paced,
#                  best-effort CPU/memory feedback; a numeric value is a fixed cap.
# --all-cases      every contrib case runs its impls.
# --sample-cases   sample at the default cap ($DEFAULT_CASE_COUNT). This
#                  is already the default for this corpus; the flag is
#                  accepted so ci/all.sh can pass it uniformly.
# --case-count=<N> sample at N cases instead.
# --case-seed=<S>  pin the draw for reproduction.
#
# Anything after `--` is forwarded verbatim to ci/run-tests.sh as
# positional case-name filters (e.g. `-- octocat-123`).
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# Absolute-path source so the helpers resolve from any cwd; shellcheck
# can't follow the dynamic $SCRIPT_DIR path (SC1091).
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

TAB=$(printf '\t')

CONTRIB_DIR=$REPO_ROOT/test-data/contrib

set_default_build_cache_size

# Per-bucket cap when this corpus samples. Contrib is a flat corpus (one
# bucket), so it is simply the number of cases that build and run. Sized
# to bound what an influx of large contributions can cost a single pass
# while leaving a small corpus running whole.
DEFAULT_CASE_COUNT=10

# Contrib samples by default: the corpus grows from outside the repo and
# is not the regression net for anything (a contrib case that exposes a
# bug is pinned by a golden).
#
# Consumed across the source boundary by lib/common.sh (SC2034).
# shellcheck disable=SC2034
CASE_MODE=sample
# shellcheck disable=SC2034
CASE_COUNT=$DEFAULT_CASE_COUNT
# shellcheck disable=SC2034
CASE_SEED=

# Configured implementations: one record per line, tab-separated as
# name<TAB>kio<TAB>runner<TAB>target. Names must not contain
# whitespace or commas.
#
# IMPL-LIST PARITY RULE — every test harness that defines a run-tests
# impl list keeps the same shape across harnesses: every kio compiler /
# backend pair the repo ships shows up in each harness's $ALL_IMPLS
# unless that harness deliberately scopes itself narrower. Contrib
# cases exercise the full backend pipeline (build + run), so the impl
# list matches `castle-tests.sh`'s. The kio-prime impl is omitted:
# contrib cases are natural surface packages, so a Kio'-only compiler
# has no dedicated source-execution bucket here (the per-case
# kio-prime-roundtrip check still verifies the Kio' boundary). See
# `TESTING.md` § Harness impl-list parity for the rule.
ALL_IMPLS="kio@js${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-js${TAB}js
kio@ts${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-ts${TAB}ts
kio@python${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-python${TAB}python
kio@java${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-java${TAB}java
kio@rust${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-rust${TAB}rust
kio@go${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-go${TAB}go
kio@swift${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-swift${TAB}swift
kio@haskell${TAB}${REPO_ROOT}/kio-rs/target/debug/kio${TAB}${REPO_ROOT}/ci/infra/kio-test-runner-rs/target/debug/kio-test-runner-haskell${TAB}haskell"

# ONLY is read by the sourced lib helpers (apply_impls_spec sets it;
# filter_impls / validate_impls_against_list consume it), not in this
# file, so shellcheck sees only the write.
# shellcheck disable=SC2034
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

while [ $# -gt 0 ]; do
  case "$1" in
    --impls=*) apply_impls_spec "${1#--impls=}" ;;
    --impls)
      shift
      [ $# -gt 0 ] || { printf 'error: --impls requires a value\n' >&2; exit 2; }
      apply_impls_spec "$1"
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
    --all-cases) apply_all_cases ;;
    --sample-cases) apply_sample_cases ;;
    --case-count=*) apply_case_count "${1#--case-count=}" ;;
    --case-count)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-count requires a value\n' >&2; exit 2; }
      apply_case_count "$1"
      ;;
    --case-seed=*) apply_case_seed "${1#--case-seed=}" ;;
    --case-seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --case-seed requires a value\n' >&2; exit 2; }
      apply_case_seed "$1"
      ;;
    -h|--help)
      regular=$(printf '%s\n' "$ALL_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
      cat <<EOF
Usage: sh $0 [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--jobs=<N>] [--compiler-jobs=<N>] [--all-cases | --sample-cases | --case-count=<N>] [--case-seed=<S>] [-- <name>...]

Build the configured Kio implementations + the Kio' verifier, then run
ci/run-tests.sh over test-data/contrib with per-case checks wired in
(kio fmt idempotence, IS_KIO_PRIME biconditional, Kio' round-trip,
TypeScript-strict typecheck, rlib-cache warm-hit).

Contrib cases are community-contributed Kio — the one lane where
external pull requests are accepted (CONTRIBUTING.md § Contribute an
example program or library). Each carries exactly one of run.args
(program), run.test-only (library), or run.sh (reviewed script);
a bug reproducer adds a KNOWN_FAILING marker. expected.exit 0, named
<github-username>-<issue-number>.

Implementation coverage (which IMPLS run):
--impls=FULL_IMPL_MATRIX runs all configured impls (default).
--impls=SAMPLE_IMPL forwards to run-tests so each case runs on one
applicable impl, picked at random per run. Use for local iteration.
--impls=<list> restricts to the named impls (comma-separated).
See TESTING.md § Local iteration.

--jobs is forwarded to run-tests as --jobs; default is run-tests'
auto = native scheduler available parallelism.
--compiler-jobs caps top-level Cargo invocations, compiler-producing Kio
commands, and actual native compiler commands across participating worktrees;
omission uses paced, best-effort CPU/memory feedback; a numeric value is a fixed cap.

Case coverage (which CONTRIB CASES run their impls) — this corpus
SAMPLES by default: it grows from outside the repo, without bound, and
it is not the regression net for anything (a contrib case that exposes
a bug is pinned by a focused golden, which guards the fix from then on):
--all-cases     every contrib case builds and runs.
--sample-cases  sample at the default cap ($DEFAULT_CASE_COUNT); already
                the default here, accepted so ci/all.sh can pass it
                uniformly across corpora.
--case-count=<N>
                sample at N cases instead.
--case-seed=<S> pin the draw. run-tests prints the effective seed and
                the selected cases.

A sampled-out case still runs every \`# ROUTING: case-binary\` check:
sampling scopes the build+run tier only.

Anything after \`--\` is forwarded to ci/run-tests.sh as positional
case-name filters (e.g. \`-- octocat-123\`).

Impls: ${regular}
EOF
      exit 0
      ;;
    --) shift; break ;;
    -*) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    *) printf 'error: unknown argument: %s (case-name filters go after --)\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

# After this loop, "$@" holds the passthrough case-name filters
# (everything after --). Save them so run-tests.sh can append them to
# its argv after the option flags.
resolve_compiler_jobs
passthrough_count=$#
init_orchestrator_tmp contrib-tests
PASSTHROUGH_FILE="$ORCHESTRATOR_TMP/passthrough.args"
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_contrib_tests() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$ORCHESTRATOR_TMP"
  exit "$cleanup_status"
}
trap cleanup_contrib_tests EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
: >"$PASSTHROUGH_FILE"
i=0
while [ "$i" -lt "$passthrough_count" ]; do
  printf '%s\n' "$1" >>"$PASSTHROUGH_FILE"
  shift
  i=$((i + 1))
done

validate_contrib_contract() {
  ok=1

  if [ ! -d "$CONTRIB_DIR" ]; then
    printf 'error: contrib corpus directory is missing: test-data/contrib\n' >&2
    return 1
  fi

  if [ ! -f "$CONTRIB_DIR/README.md" ]; then
    printf 'error: contrib corpus contract file is missing: test-data/contrib/README.md\n' >&2
    ok=0
  fi

  for dir in "$CONTRIB_DIR"/*/; do
    [ -d "$dir" ] || continue
    rel=${dir#"$REPO_ROOT"/}
    rel=${rel%/}
    name=$(basename "$dir")

    # The directory name credits the contributor and ties the case to
    # the issue that reserved it. See test-data/contrib/README.md
    # § Directory naming.
    if ! printf '%s\n' "$name" | grep -Eq '^[a-z0-9][a-z0-9-]*-[0-9]+$'; then
      printf 'error: contrib case %s must be named <github-username>-<issue-number>, lowercase\n' "$rel" >&2
      ok=0
    fi

    # A contrib case is a direct child of test-data/contrib/. Reject
    # any nested case directory (identified by an expected.exit below
    # the top level) so the flat-corpus contract can't silently erode.
    nested=$(find "$dir" -mindepth 2 -name expected.exit -type f 2>/dev/null | head -n 1)
    if [ -n "$nested" ]; then
      printf 'error: contrib case %s has a nested case directory (%s); contrib cases are direct children of test-data/contrib/\n' \
        "$rel" "${nested#"$REPO_ROOT"/}" >&2
      ok=0
    fi

    if [ ! -f "$dir/README.md" ]; then
      printf 'error: contrib case %s is missing README.md\n' "$rel" >&2
      ok=0
    fi
    if [ ! -d "$dir/workdir" ]; then
      printf 'error: contrib case %s is missing workdir/ (the Kio package)\n' "$rel" >&2
      ok=0
    elif ! ls "$dir/workdir"/*.pkg.kio >/dev/null 2>&1; then
      printf 'error: contrib case %s has no <pkg>.pkg.kio at the top of workdir/\n' "$rel" >&2
      ok=0
    fi
    exec_files=0
    exec_kind=
    [ -f "$dir/run.args" ] && { exec_files=$((exec_files + 1)); exec_kind='args'; }
    [ -f "$dir/run.sh" ] && { exec_files=$((exec_files + 1)); exec_kind='sh'; }
    [ -f "$dir/run.test-only" ] && { exec_files=$((exec_files + 1)); exec_kind='testonly'; }
    if [ "$exec_files" -ne 1 ]; then
      printf 'error: contrib case %s must carry exactly one execution file (run.args, run.sh, or run.test-only)\n' "$rel" >&2
      ok=0
    fi
    # A run.test-only case is a library (no main): it must prove something,
    # so require at least one equiv law that `kio test` will discharge.
    if [ "$exec_kind" = testonly ] && [ -d "$dir/workdir" ]; then
      equiv_found=$(find "$dir/workdir" -type f -name '*.kio' ! -path '*/out/*' \
        -exec grep -lE '^[[:space:]]*equiv[[:space:]]' {} + 2>/dev/null | head -n 1)
      if [ -z "$equiv_found" ]; then
        printf 'error: contrib case %s is run.test-only (a library) but declares no equiv law; a library must assert at least one\n' "$rel" >&2
        ok=0
      fi
    fi
    # A run.test-only library has no runner, so the harness never diffs its
    # stdout; require it empty so a mis-set golden can't pass unchecked.
    if [ "$exec_kind" = testonly ] && [ -s "$dir/expected.stdout" ]; then
      printf 'error: contrib case %s is run.test-only (a library, no runner) so expected.stdout must be empty\n' "$rel" >&2
      ok=0
    fi
    for f in expected.stdout expected.exit; do
      if [ ! -f "$dir/$f" ]; then
        printf 'error: contrib case %s is missing %s\n' "$rel" "$f" >&2
        ok=0
      fi
    done
    stderr_policies=0
    for f in expected.stderr expected.stderr.ignore expected.stderr.grep; do
      [ -f "$dir/$f" ] && stderr_policies=$((stderr_policies + 1))
    done
    if [ "$stderr_policies" -ne 1 ]; then
      printf 'error: contrib case %s must carry exactly one stderr-policy file (expected.stderr, expected.stderr.ignore, or expected.stderr.grep)\n' "$rel" >&2
      ok=0
    fi
    # expected.exit pins the *desired* outcome, which is success. A normal
    # case succeeds; a KNOWN_FAILING (bug-reproducer) case records the exit
    # it should reach once the bug is fixed — also 0.
    if [ -f "$dir/expected.exit" ] && ! expected_exit_is_zero "$dir/expected.exit"; then
      printf 'error: contrib case %s expected.exit must contain exactly 0 (the desired successful outcome)\n' "$rel" >&2
      ok=0
    fi

    # Self-contained: a contrib case brings no dependencies, so review
    # of a contrib PR stays scoped to the case directory.
    if [ -d "$dir/workdir" ]; then
      dep=$(find "$dir/workdir" -type f -name '*.dep.kio' 2>/dev/null | head -n 1)
      if [ -n "$dep" ]; then
        printf 'error: contrib case %s declares a dependency (%s); contrib cases are self-contained\n' \
          "$rel" "${dep#"$REPO_ROOT"/}" >&2
        ok=0
      fi

      # A contrib case that reads stdin via read_ascii_line() must ship
      # the input.stdin fixture; otherwise the standard runner path has
      # nothing to redirect and the program hits EOF on the first read.
      reads_stdin=$(
        find "$dir/workdir" -type f -name '*.kio' ! -path '*/out/*' -print 2>/dev/null \
          | while IFS= read -r src; do
              if grep -qE 'read_ascii_line[[:space:]]*\(' "$src" 2>/dev/null; then
                printf '%s\n' "$src"
                break
              fi
            done \
          | head -n 1
      )
      if [ -n "$reads_stdin" ] && [ ! -f "$dir/input.stdin" ]; then
        printf 'error: contrib case %s declares read_ascii_line() but has no input.stdin fixture\n' "$rel" >&2
        ok=0
      fi

      # Under workdir/ only .kio sources are allowed. This rejects
      # committed binaries, scripts, or build output (out/ excepted: it
      # exists only in a dirty local tree and is never committed) that
      # would otherwise ride into the corpus as untrusted content.
      nonkio=$(find "$dir/workdir" -type f ! -path '*/out/*' ! -name '*.kio' 2>/dev/null | head -n 1)
      if [ -n "$nonkio" ]; then
        printf 'error: contrib case %s has a non-.kio file under workdir (%s); workdir contains only .kio sources\n' \
          "$rel" "${nonkio#"$REPO_ROOT"/}" >&2
        ok=0
      fi
    fi

    # File allowlist. A contrib case contains only its control files
    # plus workdir/; a stray file, an extra directory (a nested case),
    # or a symlink (an out-of-tree escape) is rejected so no untrusted
    # content rides in alongside the .kio sources.
    sym=$(find "$dir" -type l ! -path '*/out/*' 2>/dev/null | head -n 1)
    if [ -n "$sym" ]; then
      printf 'error: contrib case %s contains a symlink (%s); contrib cases contain only regular files\n' \
        "$rel" "${sym#"$REPO_ROOT"/}" >&2
      ok=0
    fi
    entries_file=$(mktemp)
    find "$dir" -mindepth 1 -maxdepth 1 >"$entries_file" 2>/dev/null || true
    while IFS= read -r entry; do
      [ -n "$entry" ] || continue
      ename=${entry##*/}
      if [ -L "$entry" ]; then
        continue
      elif [ -d "$entry" ]; then
        if [ "$ename" != workdir ]; then
          printf 'error: contrib case %s has an unexpected directory (%s); a contrib case has only its top-level files and workdir/\n' \
            "$rel" "$ename" >&2
          ok=0
        fi
      else
        case "$ename" in
          README.md|run.args|run.sh|run.test-only|expected.stdout|expected.exit|expected.stderr|expected.stderr.ignore|expected.stderr.grep|input.stdin|IS_KIO_PRIME|KNOWN_FAILING) ;;
          *)
            printf 'error: contrib case %s has an unexpected file (%s); allowed: README.md, run.args, run.sh, run.test-only, expected.stdout, expected.exit, one expected.stderr* policy, input.stdin, IS_KIO_PRIME, KNOWN_FAILING\n' \
              "$rel" "$ename" >&2
            ok=0
            ;;
        esac
      fi
    done <"$entries_file"
    rm -f "$entries_file"
  done

  [ "$ok" = 1 ]
}

validate_contrib_contract

# Validate the --impls selection, then filter to the selected impls.
# Both helpers live in lib/common.sh.
resolve_case_seed
validate_impls_against_list "$ALL_IMPLS"
SELECTED=$(filter_impls "$ALL_IMPLS")

# Enumerate contrib cases (directories carrying the expected.exit case
# marker ci/run-tests.sh uses). An empty corpus is a clean pass and
# exits here, before the build phase, so the empty lane costs the gate
# nothing.
case_count=0
for dir in "$CONTRIB_DIR"/*/; do
  [ -d "$dir" ] || continue
  [ -f "$dir/expected.exit" ] || continue
  case_count=$((case_count + 1))
done
if [ "$case_count" = 0 ]; then
  printf 'no contrib cases (test-data/contrib/ has no cases)\n'
  exit 0
fi

if [ -z "$SELECTED" ]; then
  exit 0
fi

# Build phase. Same set of builds as `castle-tests.sh`: the kio binary
# from the kio-rs crate, the standalone Kio' grammar verifier, and the
# per-backend test runners.
needs_compiler_build=0
case "$SELECTED" in
  *kio@*) needs_compiler_build=1 ;;
esac
if [ "$needs_compiler_build" = 1 ]; then
  build_corpus_tool_binary \
    kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$ORCHESTRATOR_TMP/kio" \
    --all-features --bins
  SELECTED=$(printf '%s\n' "$SELECTED" | awk -F"$TAB" -v OFS="$TAB" \
    -v original="$REPO_ROOT/kio-rs/target/debug/kio" -v kio="$ORCHESTRATOR_TMP/kio" '
      NF > 0 { if ($2 == original) $2 = kio; print }
    ')
fi

build_corpus_tool_binary \
  kio-prime "$REPO_ROOT/kio-rs" kio-prime "$ORCHESTRATOR_TMP/kio-prime" \
  --no-default-features --features prime,cli,parallel --bin kio-prime
KIO_PRIME_BIN="$ORCHESTRATOR_TMP/kio-prime"

build_corpus_tool_binary \
  kio-prime-check "$REPO_ROOT/ci/infra/kio-prime-check-rs" \
  kio-prime-check "$ORCHESTRATOR_TMP/kio-prime-check"

build_runner_feature() {
  feature=$1
  ( cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --no-default-features --features "$feature" )
}

selected_runner_targets=$(printf '%s\n' "$SELECTED" | awk -F"$TAB" 'NF > 0 { print $4 }')
for target in js ts python java rust go swift haskell; do
  if printf '%s\n' "$selected_runner_targets" | grep -qx "$target"; then
    build_runner_feature "$target"
  fi
done

KIO_PRIME_CHECK_BIN="$ORCHESTRATOR_TMP/kio-prime-check"
export KIO_PRIME_CHECK_BIN

cd "$REPO_ROOT"

# Build the run-tests.sh argv.
set -- "--cases-dir=test-data/contrib" "--cache-base=$(shared_cache_base contrib)"
TMPDIR_RT_ARGS="$ORCHESTRATOR_TMP/run-tests.args"
echo "$SELECTED" | while IFS=$TAB read -r impl_name impl_kio impl_runner impl_target; do
  [ -n "$impl_name" ] || continue
  printf -- '--impl-def=name=%s,kio=%s,runner=%s,target=%s,prime-kio=%s\n' \
    "$impl_name" "$impl_kio" "$impl_runner" "$impl_target" "$KIO_PRIME_BIN"
done >"$TMPDIR_RT_ARGS"
while IFS= read -r arg; do
  [ -n "$arg" ] || continue
  set -- "$@" "$arg"
done <"$TMPDIR_RT_ARGS"
# Per-case checks whose contracts fit contrib cases: fmt idempotence
# and the IS_KIO_PRIME biconditional on the source, artifact parity
# between the source pipeline and the Kio'-roundtripped build, the
# TypeScript-strict typecheck, and the rlib-cache warm-hit. Equiv
# discharge needs no wired check: the standard run.args path runs
# `kio test` as a build prerequisite, so an equiv-discharge regression
# fails the case anyway (the run.sh discharge gate in
# ci/checks/repo-lint/equiv-discharge.sh covers run.sh cases, which
# contrib forbids). dep-canonical.sh is omitted too: contrib cases are
# self-contained by contract (no `*.dep.kio`), so the committed-tree
# drift check would self-skip on every case. See TESTING.md § Harness
# impl-list parity.
set -- "$@" \
  "--check=$REPO_ROOT/ci/checks/per-case/fmt-canonical.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/prime-marker.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/kio-prime-roundtrip.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/rlib-cache-second-run-hits.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/tsc-strict.sh" \
  "--check=$REPO_ROOT/ci/checks/per-case/pyright-strict.sh"
if [ -n "$JOBS" ]; then
  set -- "$@" "--jobs=$JOBS"
fi
if [ "$COMPILER_JOBS" != adaptive ]; then
  set -- "$@" "--compiler-jobs=$COMPILER_JOBS"
fi
if [ "$SAMPLE_IMPL" = 1 ]; then
  set -- "$@" "--impls=SAMPLE_IMPL"
fi
while IFS= read -r arg; do
  [ -n "$arg" ] || continue
  set -- "$@" "$arg"
done <<EOF
$(case_sampling_args)
EOF
# Caller-supplied positional case-name filters (after `--`).
while IFS= read -r filter; do
  [ -n "$filter" ] || continue
  set -- "$@" "$filter"
done <"$PASSTHROUGH_FILE"

printf '\n========== run-tests (contrib) ==========\n'
sh ci/run-tests.sh "$@"
