#!/bin/sh
# ROUTING: impl
# REQUIRES: runner
#
# Per-case runner build-cache warm-hit check.
#
# Gated on the `CACHE_HIT_ON_RERUN` marker file (parallel to
# `IS_KIO_PRIME`): cases without the marker skip cleanly. Cases
# with the marker have a `build { ... }` block that declares
# `cache "<path>";` and exercise a cache-backed impl's content-
# addressed runner build cache.
#
# Applies to every runner executable that uses the shared build cache —
# `kio-test-runner-rust` (the two-level rlib/bin `RlibCache`),
# `kio-test-runner-go` (the one-level `GoCache`),
# `kio-test-runner-haskell` (the one-level `HaskellCache`), and
# `kio-test-runner-swift` (the one-level `SwiftCache`). Other runners
# (js / ts / dyn-load-prime) have no compiled binary to cache and skip.
#
# What the check asserts:
#   1. Firing — a cold runner invocation against a *fresh* cache root
#      publishes a binary entry under that root. The adapter must
#      actually populate the cache; a silently no-op cache would leave
#      the root empty and fail here.
#   2. Warm-hit correctness — a second back-to-back invocation against
#      the now-populated root produces the same exit code, stdout, and
#      stderr as the first. The cache is the only mechanism that makes
#      the second invocation cheap; if it silently produced wrong
#      output, the second invocation would diverge from the first.
#
# The check drives a fresh per-invocation cache root (not the harness's
# shared one) so the firing assertion is case-scoped and cannot be
# satisfied by another case's leftovers. It is observable-behaviour-
# scoped otherwise — it does not poke at the cache's internal key
# derivation, only that *an* entry appears.
#
# POSIX sh only.

set -eu

marker=CACHE_HIT_ON_RERUN

if [ ! -f "$marker" ]; then
  exit 0
fi

# The harness publishes the adapter's non-executable cache-kind metadata.
# An empty value means this runner has no compiled-artifact cache.
cache_kind=${KIO_TEST_RUNNER_CACHE_KIND:-}
[ -n "$cache_kind" ] || exit 0

if [ -z "${KIO_BIN:-}" ] || [ -z "${KIO_RUNNER:-}" ]; then
  printf 'rlib-cache-second-run-hits: KIO_BIN / KIO_RUNNER not set\n' >&2
  exit 2
fi

if [ -f run.sh ] || [ -f run.test-only ]; then
  exit 0
fi
if [ ! -f run.args ]; then
  printf 'rlib-cache-second-run-hits: case has none of run.args / run.sh / run.test-only\n' >&2
  exit 2
fi
if [ -z "${KIO_TEST_RUN_ARGS_FILE:-}" ]; then
  printf 'rlib-cache-second-run-hits: KIO_TEST_RUN_ARGS_FILE not set\n' >&2
  exit 2
fi

run_target() {
  rtc_out_dir=$1
  set --
  while IFS= read -r rtc_arg || [ -n "$rtc_arg" ]; do
    set -- "$@" "$rtc_arg"
  done <"$KIO_TEST_RUN_ARGS_FILE"
  set -- "$@" "$rtc_out_dir"
  if [ -f ../input.stdin ]; then
    "$KIO_RUNNER" "$@" < ../input.stdin
  else
    "$KIO_RUNNER" "$@"
  fi
}

# Scratch copy of the case so we don't pollute the gold tree with
# `out/`. The original `expected.*` files stay where they are.
scratch=$(mktemp -d)
# A fresh cache root, distinct from the harness's shared one, so the
# firing assertion below is case-scoped: the cold run must fill *this*
# root from empty.
runner_cache=$(mktemp -d)
trap 'rm -rf "$scratch" "$runner_cache"' EXIT INT TERM HUP
cp -RL . "$scratch/case"
cd "$scratch/case/workdir"

KIO_TEST_RUNNER_BUILD_CACHE_DIR="$runner_cache"
export KIO_TEST_RUNNER_BUILD_CACHE_DIR
# The cache must be enabled for this check; clear any disable the
# environment carried so a CI-wide `KIO_TEST_RUNNER_CACHE_DISABLE=1`
# can't turn the firing assertion into a no-op.
unset KIO_TEST_RUNNER_CACHE_DISABLE

"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>"$scratch/build1.err" || {
  status=$?
  printf 'rlib-cache-second-run-hits: first build failed (exit %d):\n' "$status" >&2
  cat "$scratch/build1.err" >&2
  exit 1
}

# First (cold) run against the empty cache root: must populate it.
set +e
run_target "out/$KIO_TARGET" >"$scratch/out1" 2>"$scratch/err1"
status1=$?
set -e

# Firing assertion: the cold run must have published at least one
# binary under this target's cache-kind subtree. An entry is a regular
# file inside a `<hex>/` key directory (the adapters name it `bin` /
# `lib.rlib`); finding any non-bookkeeping regular file under the kind
# root proves the adapter fired.
published=$(find "$runner_cache/$cache_kind" -type f \
  ! -name '.gitignore' ! -name '.lock' ! -name '.last_used' \
  ! -name 'meta.json' 2>/dev/null | sed -n '1p')
if [ -z "$published" ]; then
  printf 'rlib-cache-second-run-hits: cold run published no artifact under %s/%s — the cache silently no-opped\n' \
    "$runner_cache" "$cache_kind" >&2
  exit 1
fi

# Second (warm) run: served by the now-populated cache. The assertions
# below verify repeated invocations remain *correct* — the
# safety-critical property — which a wrong cache hit would break.
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>"$scratch/build2.err" || {
  status=$?
  printf 'rlib-cache-second-run-hits: second build failed (exit %d):\n' "$status" >&2
  cat "$scratch/build2.err" >&2
  exit 1
}

set +e
run_target "out/$KIO_TARGET" >"$scratch/out2" 2>"$scratch/err2"
status2=$?
set -e

if [ "$status1" != "$status2" ]; then
  printf 'rlib-cache-second-run-hits: exit code diverged across runs (first=%d, second=%d)\n' \
    "$status1" "$status2" >&2
  exit 1
fi

if ! diff -q "$scratch/out1" "$scratch/out2" >/dev/null 2>&1; then
  printf 'rlib-cache-second-run-hits: stdout diverged across runs:\n' >&2
  diff -u "$scratch/out1" "$scratch/out2" >&2 || true
  exit 1
fi

if ! diff -q "$scratch/err1" "$scratch/err2" >/dev/null 2>&1; then
  printf 'rlib-cache-second-run-hits: stderr diverged across runs:\n' >&2
  diff -u "$scratch/err1" "$scratch/err2" >&2 || true
  exit 1
fi

exit 0
