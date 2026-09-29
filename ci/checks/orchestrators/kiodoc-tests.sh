#!/bin/sh
# shellcheck disable=SC2034 # Capacity-explicitness is read by sourced common.sh.
#
# Kiodoc test orchestrator.
#
# Two phases:
#
# 1. Run the `kio doc` goldens at `test-data/kiodoc-cases/` through the
#    standard `ci/run-tests.sh` diff harness. Each case is a
#    package directory (a `pkg.pkg.kio` whose `build { ... }`
#    block declares a `docs` field, plus the markdown / `.kio` content
#    the case exercises) with a `run.sh` that invokes `$KIO_BIN doc
#    check`, `fmt`, or `build` from the case root, with `expected.exit`,
#    `expected.stdout`, and one stderr policy file.
# 2. Smoke-test: `cd docs/ && kio doc check`, then
#    `kio doc fmt --check`, against the repo's full documentation
#    tree. `docs/` carries its own `docs.pkg.kio`
#    (a `build { docs { md "." }; }` block), making it a docs-only
#    package. Every
#    snippet in `docs/` is either `{ignore}` or migrated to real
#    validation; these runs gate a docs/ change on valid and canonical
#    Kiodoc snippets.
#
# `kio` is retained from the shared full-feature compiler build. The runner= slot of
# `--impl-def=` is unused by kiodoc cases (none of them call into a host
# runtime) but `ci/run-tests.sh` requires it; we point it
# at the `kio` binary itself.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# Absolute-path source so the helpers resolve from any cwd; shellcheck
# can't follow the dynamic $SCRIPT_DIR path (SC1091).
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

# This harness is js-only: its runner populates no compiled-artifact
# cache, so it sets no KIO_TEST_RUNNER_BUILD_CACHE_SIZE cap (nothing
# accumulates to evict, unlike the Rust / Go / Haskell runners).
# shared_cache_base (from lib/common.sh) still keeps its test-runner
# cache-base off the worktree-local .kio-cache/ tree uniformly. See
# ai/topics/local-tools.md § Compiler cache.

JOBS=
if [ "${KIO_CI_SCHEDULE_COMPILER_JOBS+x}" = x ]; then
  COMPILER_JOBS=$KIO_CI_SCHEDULE_COMPILER_JOBS
  COMPILER_JOBS_EXPLICIT=1
else
  COMPILER_JOBS=adaptive
  COMPILER_JOBS_EXPLICIT=0
fi

while [ $# -gt 0 ]; do
  case "$1" in
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
    -h|--help)
      cat <<EOF
Usage: sh $0 [--jobs=<N>] [--compiler-jobs=<N>]

Run the kiodoc goldens (test-data/kiodoc-cases/) through
ci/run-tests.sh, then run \`kio doc check\` and
\`kio doc fmt --check\` inside docs/ as smoke tests against the
repo's documentation tree.

--jobs is forwarded to run-tests as --jobs; default is run-tests'
auto = native scheduler available parallelism.
--compiler-jobs caps top-level Cargo invocations, compiler-producing Kio
commands, and actual native compiler commands across participating worktrees;
omission uses paced, best-effort CPU/memory feedback; a numeric value is a fixed cap.

EOF
      exit 0
      ;;
    *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

resolve_compiler_jobs

init_orchestrator_tmp kiodoc-tests
trap 'rm -rf "$ORCHESTRATOR_TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$ORCHESTRATOR_TMP/kio" \
  --all-features --bins
KIO_BIN="$ORCHESTRATOR_TMP/kio"

# Phase 1: goldens corpus.
#
# IMPL-LIST PARITY RULE — every test harness that defines a run-tests
# impl list keeps the *same shape* across harnesses: every kio compiler /
# backend pair the repo ships shows up in each harness's impl list
# unless that harness deliberately scopes itself narrower. This
# harness is js-only because doc snippets exercise `kio doc`'s
# markdown / typecheck path, not the backends. See
# `TESTING.md` § Harness impl-list parity for the rule.
set -- "--cases-dir=$REPO_ROOT/test-data/kiodoc-cases" \
       "--cache-base=$(shared_cache_base kiodoc)" \
       "--impl-def=name=kio@js,kio=${KIO_BIN},runner=${KIO_BIN},target=js"
if [ -n "$JOBS" ]; then
  set -- "$@" "--jobs=$JOBS"
fi
if [ "$COMPILER_JOBS" != adaptive ]; then
  set -- "$@" "--compiler-jobs=$COMPILER_JOBS"
fi

printf '\n========== kiodoc-tests: run-tests over test-data/kiodoc-cases ==========\n'
cd "$REPO_ROOT"
sh ci/run-tests.sh "$@"

# Phase 2: smoke test against the repo's docs/ tree. `docs/` is a
# docs-only package (its own `docs.pkg.kio` carries a
# `build { docs { md "." }; }` block), so `kio doc check` run from
# inside it walks the markdown tree. The formatter check uses the same
# docs package context and must exit 0 too. Every kio fence in the tree
# is either {ignore} or validated; every formattable snippet is
# canonical.
# shellcheck disable=SC2016 # literal backticks in printf banner
printf '\n========== kiodoc-tests: smoke test `kio doc check` in docs/ ==========\n'
(
  cd "$REPO_ROOT/docs"
  sh "$REPO_ROOT/ci/schedule.sh" --resource compiler -- "$KIO_BIN" doc check
)

# shellcheck disable=SC2016 # literal backticks in printf banner
printf '\n========== kiodoc-tests: smoke test `kio doc fmt --check` in docs/ ==========\n'
(
  cd "$REPO_ROOT/docs"
  sh "$REPO_ROOT/ci/schedule.sh" --resource compiler -- "$KIO_BIN" doc fmt --check
)
