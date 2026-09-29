#!/bin/sh
# shellcheck disable=SC2034 # Capacity-explicitness is read by sourced common.sh.
#
# Generate a batch of Kio test cases and run the goldens
# harness against every configured implementation.
#
# kio-rs produces two release binaries: `kio` (the CLI) and
# `kio-test-runner`. kio-gen (private crate at ci/infra/kio-gen-rs/)
# produces the batch.
#
# The list of implementations is hardcoded below in $ALL_IMPLS as a
# tab-separated multi-line string (one record per line:
# name<TAB>kio<TAB>runner). Adding a new implementation = adding a
# new record plus its build step. This mirrors
# ci/checks/orchestrators/golden-tests.sh; if the duplication grows once a
# second implementation exists, the shared list can be factored into
# a tiny sourced file under ci/.
#
# Usage:
#   sh ci/checks/orchestrators/generative-tests.sh \
#     [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--count=<N>] [--seed=<N>] \
#     [--compiler-jobs=<N>] \
#     [-- <run-tests args>...]
#
# --impls=FULL_IMPL_MATRIX
#                run all configured impls (default).
# --impls=SAMPLE_IMPL
#                for each case, run exactly one applicable impl,
#                 picked at random per run.
# --impls=<list>  restrict run-tests to the named comma-separated impls.
# --count=<N>     number of programs to generate.
#                 ci/all.sh forwards its --gen-count=<N> flag as
#                 --count=<N> so a full sweep can be scoped to a
#                 smaller batch without invoking this orchestrator
#                 directly.
# --seed=<N>      deterministic seed; default is random per run.
#                 The chosen seed is logged so any failure can be
#                 reproduced.
# --compiler-jobs=<N>
#                 cap top-level Cargo invocations, compiler-producing Kio
#                 commands, and runner compiler commands; omission uses paced,
#                 best-effort CPU/memory feedback; a numeric value is a fixed cap.
#
# Anything after `--` is forwarded verbatim to ci/run-tests.sh
# (e.g., --show-output, --jobs=, case-name filters).
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

set_default_build_cache_size

# Configured implementations: one record per line, tab-separated as
# name<TAB>kio<TAB>runner<TAB>target. Names must not contain
# whitespace or commas.
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
# PARITY EXCEPTION (per the IMPL-LIST PARITY RULE above): this harness
# runs only the two regular impls. Like the kio-prime@js Kio'-only group
# golden-tests.sh runs over IS_KIO_PRIME cases (also absent here), the
# dyn-load-prime@kio-prime differential group golden-tests.sh runs over
# DYN_LOAD_PRIME cases is omitted: both are marker-gated disjoint passes, and
# kio-gen marks no case with either. The dyn_load_prime loader now loads the
# grammar kio-gen emits — newtypes / type aliases, qualified constructor /
# projector calls, multi-module packages, and the multi-parameter /
# polymorphic-lambda dictionary-passing shapes elaborators expand to. What
# remains is the dyn-load-prime value model: the interpreter's host scalars are
# i32 / string / bool only (`test-data/poc/dyn_load_prime/workdir/prime.kio` § Hostval), so a
# generated program using a wider numeric (i64 / i128 / u64 / u128) or a
# float literal is outside it. Wiring this group therefore needs a kio-gen
# generation mode that both marks DYN_LOAD_PRIME and restricts a case's host
# scalars to i32 / string / bool (or a wider dyn-load-prime value model); once
# one exists, add the group and its driver build mirroring golden-tests.sh.
# See TESTING.md § Harness impl-list parity.

# DEFAULT_COUNT bounds the seeded differential-fuzz batch. This harness is a
# cross-impl fuzzer, not a coverage run: kio-gen's marginal line coverage over
# the goldens plateaus by ~100 generated programs, so N trades fuzzing breadth
# (fresh data/shapes over already-covered lines, where cross-impl divergences
# hide) against CI wall-clock rather than buying coverage. 200 sits
# deliberately past that plateau while keeping the batch cheap; raise it via
# --count= for a deeper differential sweep.
ONLY=
DEFAULT_COUNT=200
COUNT=$DEFAULT_COUNT
SEED=
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
    --count=*) COUNT=${1#--count=} ;;
    --count)
      shift
      [ $# -gt 0 ] || { printf 'error: --count requires a value\n' >&2; exit 2; }
      COUNT=$1
      ;;
    --seed=*) SEED=${1#--seed=} ;;
    --seed)
      shift
      [ $# -gt 0 ] || { printf 'error: --seed requires a value\n' >&2; exit 2; }
      SEED=$1
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
    -h|--help)
      configured=$(printf '%s\n' "$ALL_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
      cat <<EOF
Usage: sh $0 [--impls=FULL_IMPL_MATRIX|SAMPLE_IMPL|<name>[,<name>...]] [--count=<N>] [--seed=<N>] [--compiler-jobs=<N>] [-- <run-tests args>...]

Generate a batch of Kio test cases (via kio-gen) and run
them through the configured Kio implementations via ci/run-tests.sh.

--impls=FULL_IMPL_MATRIX runs all configured impls (default).
--impls=SAMPLE_IMPL forwards to run-tests so each case runs on one
applicable impl, picked at random per run. Use for local iteration.
--impls=<list> restricts to the named impls (comma-separated).
See TESTING.md § Local iteration.

--count specifies how many programs to generate (default: $DEFAULT_COUNT).
ci/all.sh forwards its --gen-count=<N> flag as --count=<N> so a full
sweep can be scoped to a smaller batch without invoking this
orchestrator directly.
--seed pins the generator's seed (default: random per run; the
seed is logged so any failure can be reproduced).
--compiler-jobs caps top-level Cargo invocations, compiler-producing Kio
commands, and actual native compiler commands across participating worktrees;
omission uses paced, best-effort CPU/memory feedback; a numeric value is a fixed cap.


Anything after \`--\` is forwarded verbatim to ci/run-tests.sh.

Configured impls: ${configured}
EOF
      exit 0
      ;;
    --) shift; break ;;
    -*) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    *) printf 'error: unknown argument: %s (case-name filters go after --)\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

resolve_compiler_jobs

# Compute SELECTED_IMPLS from $ALL_IMPLS, filtered by $ONLY if set.
#
# NB: multi-line strings are passed via env vars and read with
# ENVIRON[] rather than awk's `-v key=value`. BSD awk on macOS
# rejects newlines in -v values with "newline in string …"; ENVIRON
# is the POSIX-portable workaround.
if [ -z "$ONLY" ]; then
  SELECTED_IMPLS=$ALL_IMPLS
else
  unknown=$(IMPL_ALL=$ALL_IMPLS IMPL_ONLY=$ONLY awk -F"$TAB" '
    BEGIN {
      all = ENVIRON["IMPL_ALL"]
      only = ENVIRON["IMPL_ONLY"]
      n = split(all, lines, "\n")
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
  if [ -n "$unknown" ]; then
    configured=$(printf '%s\n' "$ALL_IMPLS" | awk -F"$TAB" 'NF>0 {printf "%s ", $1}')
    printf 'error: --impls: unknown impl(s):%s (configured: %s)\n' \
      "$unknown" "$configured" >&2
    exit 2
  fi

  SELECTED_IMPLS=$(IMPL_ALL=$ALL_IMPLS IMPL_ONLY=$ONLY awk -F"$TAB" '
    BEGIN {
      all = ENVIRON["IMPL_ALL"]
      only = ENVIRON["IMPL_ONLY"]
      m = split(only, want, ",")
      for (i = 1; i <= m; i++) want_set[want[i]] = 1
      n = split(all, lines, "\n")
      for (i = 1; i <= n; i++) {
        if (lines[i] == "") continue
        split(lines[i], rec, "\t")
        if (rec[1] in want_set) print lines[i]
      }
    }
  ')
fi

# Keep the generated cases and compiler snapshot in one private lifecycle.
init_orchestrator_tmp generative-tests
batch="$ORCHESTRATOR_TMP/batch"
mkdir "$batch"
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_generative_tests() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$ORCHESTRATOR_TMP"
  exit "$cleanup_status"
}
trap cleanup_generative_tests EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# Build the impls in $SELECTED_IMPLS that need building. Today every
# slot is served by the kio-rs crate (`<binary>@<target>` shape),
# so a single `cargo build` covers every selected kio@<target> impl.
# Expand this case as new impls driven by other crates are added.
needs_compiler_build=0
while IFS=$TAB read -r impl_name _ _; do
  [ -n "$impl_name" ] || continue
  case "$impl_name" in
    kio@*|kio-prime@*) needs_compiler_build=1 ;;
  esac
done <<EOF
$SELECTED_IMPLS
EOF
if [ "$needs_compiler_build" = 1 ]; then
  build_corpus_tool_binary \
    kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$ORCHESTRATOR_TMP/kio" \
    --all-features --bins
  SELECTED_IMPLS=$(printf '%s\n' "$SELECTED_IMPLS" | awk -F"$TAB" -v OFS="$TAB" \
    -v original="$REPO_ROOT/kio-rs/target/debug/kio" -v kio="$ORCHESTRATOR_TMP/kio" '
      NF > 0 { if ($2 == original) $2 = kio; print }
    ')
fi

# Build kio-gen (ci profile).
( cd "$REPO_ROOT/ci/infra/kio-gen-rs" && sh "$REPO_ROOT/ci/cargo.sh" build )

# Build kio-prime-check-rs (ci profile) — the standalone Kio' grammar
# verifier used by ci/checks/per-case/prime-marker.sh as a post-hoc
# cross-check on the generator's IS_KIO_PRIME claim.
( cd "$REPO_ROOT/ci/infra/kio-prime-check-rs" && sh "$REPO_ROOT/ci/cargo.sh" build )

# Build only the runner bins selected by --impls. Per-feature builds
# match the hygiene script's discipline so cross-bin dead_code analysis
# stays honest (see ci/checks/hygiene/kio-test-runner-rs.sh).
build_runner_feature() {
  feature=$1
  ( cd "$REPO_ROOT/ci/infra/kio-test-runner-rs" && sh "$REPO_ROOT/ci/cargo.sh" build --no-default-features --features "$feature" )
}

selected_runner_targets=$(printf '%s\n' "$SELECTED_IMPLS" | awk -F"$TAB" 'NF > 0 { print $4 }')
for target in js ts python java rust go swift haskell; do
  if printf '%s\n' "$selected_runner_targets" | grep -qx "$target"; then
    build_runner_feature "$target"
  fi
done

# Generate the batch inside the owned temporary root.
if [ -n "$SEED" ]; then
  "$REPO_ROOT/ci/infra/kio-gen-rs/target/debug/kio-gen" \
    --output "$batch" --count "$COUNT" --seed "$SEED"
else
  "$REPO_ROOT/ci/infra/kio-gen-rs/target/debug/kio-gen" \
    --output "$batch" --count "$COUNT"
fi

# Build the final argv for run-tests.sh as:
#   --cases-dir=...  --impl-def=... [--impl-def=...]...  <passthrough>
#
# Passthrough is currently in "$@". Append our defaults + impls to the
# end, then rotate the leading $passthrough_count args to the back.
passthrough_count=$#

set -- "$@" "--cases-dir=$batch" "--cache-base=$(shared_cache_base kio-gen)"
while IFS=$TAB read -r impl_name impl_kio impl_runner impl_target; do
  [ -n "$impl_name" ] || continue
  set -- "$@" \
    "--impl-def=name=${impl_name},kio=${impl_kio},runner=${impl_runner},target=${impl_target}"
done <<EOF
$SELECTED_IMPLS
EOF

# Wire prime-marker.sh into the per-case check pipeline. The check
# enforces the biconditional `IS_KIO_PRIME ⇔ all *.kio parse as Kio'`,
# so if the generator's `prog.uses_surface ⇒ marker` plumbing ever
# drifts from the formal Kio' grammar boundary, the cross-check
# catches it on the freshly-generated batch.
set -- "$@" "--check=$REPO_ROOT/ci/checks/per-case/prime-marker.sh"

# tsc-strict.sh type-checks the TS backend's emitted `<pkg>.d.ts` skin
# (gated on the kio@ts impl); pyright-strict.sh type-checks the Python
# backend's emitted `<pkg>/` stub package (gated on the kio@python impl). A
# generated program that builds emits both, which must pass their strict
# type-checks.
set -- "$@" "--check=$REPO_ROOT/ci/checks/per-case/tsc-strict.sh"
set -- "$@" "--check=$REPO_ROOT/ci/checks/per-case/pyright-strict.sh"

if [ "$COMPILER_JOBS" != adaptive ]; then
  set -- "$@" "--compiler-jobs=$COMPILER_JOBS"
fi
if [ "$SAMPLE_IMPL" = 1 ]; then
  set -- "$@" "--impls=SAMPLE_IMPL"
fi

i=0
while [ "$i" -lt "$passthrough_count" ]; do
  arg=$1
  shift
  set -- "$@" "$arg"
  i=$((i + 1))
done

KIO_PRIME_CHECK_BIN="$REPO_ROOT/ci/infra/kio-prime-check-rs/target/debug/kio-prime-check"
export KIO_PRIME_CHECK_BIN

cd "$REPO_ROOT"
sh ci/run-tests.sh "$@"
