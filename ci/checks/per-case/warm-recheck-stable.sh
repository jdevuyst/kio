#!/bin/sh
# ROUTING: case-binary
#
# Per-case warm-recheck stability check.
#
# Gated on the `WARM_REBUILD_STABLE` marker file (parallel to
# `IS_KIO_PRIME` and `CACHE_HIT_ON_RERUN`): cases without the marker
# skip cleanly via the autotools 77 convention.
#
# What it asserts: a second back-to-back `kio check` against the same
# package — the warm run, served by the Kio-semantic caches — produces
# the *same* exit code, stdout, and stderr as the first (cold) run. The
# motivating regression is a cache-poisoning bug in the error path: a
# typecheck failure leaves the failing module un-cached while its
# dependencies (including user-elaborator-defining modules) are cached;
# on the warm rerun the failing module is retyped against the cached
# dependencies' un-forced placeholder bodies, so a user elaborator
# evaluated to `()` instead of its real body and the warm run reported a
# different (wrong) diagnostic. The cache must be correctness-
# transparent for error cases: cold and warm must agree.
#
# The check is observable-behaviour-scoped and backend-agnostic — it
# exercises only `kio check` (the front-end, shared by every impl) and
# never inspects cache internals, so it does not couple the goldens to
# kio-rs implementation details.
#
# Invoked once per (case, binary) by ci/run-tests.sh's --check pipeline.
# cwd is the original case directory; KIO_BIN is the compiler binary.
#
# POSIX sh only.

set -u

marker=WARM_REBUILD_STABLE

if [ ! -f "$marker" ]; then
  exit 77
fi

if [ -z "${KIO_BIN:-}" ]; then
  printf 'warm-recheck-stable: KIO_BIN is not set\n' >&2
  exit 2
fi

if [ ! -d workdir ]; then
  printf 'warm-recheck-stable: case has no workdir/ (package files live there)\n' >&2
  exit 2
fi

# A Kio'-only compiler rejects the surface `check` subcommand; skip.
if [ -f IS_KIO_PRIME ]; then
  exit 77
fi

# Scratch copy of the case so the gold tree's `out/` stays clean and the
# cold run starts from a genuinely empty cache regardless of any other
# check's leftovers.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -RL . "$scratch/case"
cd "$scratch/case/workdir" || {
  printf 'warm-recheck-stable: could not enter scratch workdir\n' >&2
  exit 2
}

# Cold: clear any cache the scratch copy carried, then check. The corpus
# harness shares typed-module entries across ordinary units in one run; this
# cache-specific check deliberately shadows that root so its first check stays
# genuinely cold.
KIO_DEBUG_TYPED_CACHE_ROOT='' "$KIO_BIN" cache clear >/dev/null 2>&1 || true

set +e
KIO_DEBUG_TYPED_CACHE_ROOT='' "$KIO_BIN" check >"$scratch/out1" 2>"$scratch/err1"
status1=$?
KIO_DEBUG_TYPED_CACHE_ROOT='' "$KIO_BIN" check >"$scratch/out2" 2>"$scratch/err2"
status2=$?
set -e

if [ "$status1" != "$status2" ]; then
  printf 'warm-recheck-stable: exit code diverged cold->warm (cold=%d, warm=%d)\n' \
    "$status1" "$status2" >&2
  printf -- '--- cold stderr ---\n' >&2
  cat "$scratch/err1" >&2
  printf -- '--- warm stderr ---\n' >&2
  cat "$scratch/err2" >&2
  exit 1
fi

if ! diff -q "$scratch/out1" "$scratch/out2" >/dev/null 2>&1; then
  printf 'warm-recheck-stable: stdout diverged cold->warm:\n' >&2
  diff -u "$scratch/out1" "$scratch/out2" >&2 || true
  exit 1
fi

if ! diff -q "$scratch/err1" "$scratch/err2" >/dev/null 2>&1; then
  printf 'warm-recheck-stable: stderr diverged cold->warm:\n' >&2
  diff -u "$scratch/err1" "$scratch/err2" >&2 || true
  exit 1
fi

exit 0
