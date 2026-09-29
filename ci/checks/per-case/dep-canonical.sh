#!/bin/sh
# ROUTING: case-binary
#
# Per-case committed-materialized-tree canonicality check.
#
# Invoked once per (case, binary) by ci/run-tests.sh's --check
# pipeline (routing marker above). cwd is the original case
# directory. The runner sets:
#
#   KIO_BIN          — the kio compiler binary for this (case, binary)
#                       unit. The check runs that binary's `kio dep
#                       fetch`; impls sharing the binary share the answer.
#   KIO_TEST_UPDATE  — "1" when run-tests.sh is in -u/--update-expected
#                       mode. The check then regenerates the trees in
#                       place (so the diff can be committed) instead of
#                       failing on drift.
#
# Contract: a package reused as a dependency commits its materialized
# transitive closure (the re-rooted `<local>/…` module tree). This check
# regenerates that tree with `kio dep fetch` and asserts the committed
# tree is byte-for-byte what fetch produces — present, canonical, no
# drift. A drift (a stale, missing, or extra committed module) is a
# finding: the committed tree no longer matches the dependency source.
#
# Out of scope: a case with no `*.dep.kio` (nothing to materialize), and
# a case carrying a `SKIP_DEP_MATERIALIZED` marker (its dependency is
# intentionally non-materializable, so there is no canonical tree).
#
# POSIX sh only.

set -u

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091 # absolute path derived from this script
. "$SCRIPT_DIR/../../infra/dep-materialization-order.sh"

if [ -z "${KIO_BIN:-}" ]; then
  printf 'dep-canonical: KIO_BIN is not set\n' >&2
  exit 2
fi

if [ ! -d workdir ]; then
  exit 0
fi

if [ -f SKIP_DEP_MATERIALIZED ] || [ -f workdir/SKIP_DEP_MATERIALIZED ]; then
  exit 0
fi

# No declared dependency -> nothing to materialize. Dependency files may live
# in nested packages such as workdir/demo/library.dep.kio.
deps=$(find workdir -name '*.dep.kio' -type f -print | sort)
[ -n "$deps" ] || exit 0

# Regenerate the materialized trees from the dependency source.
#
# `--force` is load-bearing: `kio dep fetch` skips re-materializing a
# dependency whose tree on disk already matches the lock (the
# skip-if-already-materialized fast path). Without `--force` the drift
# check would degrade to "committed == committed" (a no-op fetch rewrites
# nothing, so the comparison below trivially passes) instead of "committed
# == fresh fetch output". `--force` re-materializes unconditionally, so the
# comparison is a genuine regeneration.
#
# A non-zero fetch is fatal (not swallowed with `|| :`): a fetch that fails
# before writing would leave the committed tree unchanged, so an empty
# `git status` below would falsely pass — asserting nothing. A committed-dep
# case is expected to materialize cleanly (a non-materializable fixture
# carries `SKIP_DEP_MATERIALIZED`, handled above), so a fetch failure here
# is a real finding.
dep_dirs=$(printf '%s\n' "$deps" | sed 's|/[^/]*$||' | sort -u)
ordered_dep_dirs=$(dep_materialization_order "$dep_dirs") || exit 1
for dep_dir in $ordered_dep_dirs; do
  if ! ( cd "$dep_dir" && "$KIO_BIN" dep fetch --force >/dev/null 2>&1 ); then
    # shellcheck disable=SC2016 # backticks are literal in the user message
    printf 'dep-canonical: `kio dep fetch --force` failed during regeneration in %s\n' "$dep_dir" >&2
    printf '(the committed dependency tree could not be regenerated from source)\n' >&2
    exit 1
  fi
done

if [ "${KIO_TEST_UPDATE:-0}" = 1 ]; then
  # In update mode the regenerated trees are left in place for the
  # caller to commit; canonicality is whatever fetch just wrote.
  exit 0
fi

# Drift detection: compare the just-fetched on-disk tree against the
# committed tree. The case lives in the real working checkout, so
# `git status --porcelain` over each `<local>/` path reports any
# difference between HEAD and the regenerated tree (modified, added, or
# deleted module). An empty report means the committed tree is canonical.
repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || {
  printf 'dep-canonical: not inside a git checkout; cannot compare committed tree\n' >&2
  exit 2
}

case_dir=$(pwd)
drift=""
for d in $deps; do
  local_name=$(basename "$d" .dep.kio)
  tree="$case_dir/$(dirname "$d")/$local_name"
  [ -d "$tree" ] || continue
  status=$(git -C "$repo_root" status --porcelain -- "$tree" 2>/dev/null)
  [ -n "$status" ] && drift="$drift$status
"
done

if [ -n "$drift" ]; then
  # shellcheck disable=SC2016 # backticks are literal in the user message
  printf 'dep-canonical: committed materialized tree drifted from `kio dep fetch` output\n' >&2
  printf '(regenerate and commit: rerun ci/checks/repo-lint/dep-materialization.sh --write):\n' >&2
  printf '%s' "$drift" | sed 's/^/  /' >&2
  exit 1
fi

exit 0
