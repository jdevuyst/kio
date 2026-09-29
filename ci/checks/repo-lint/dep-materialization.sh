#!/bin/sh
#
# Materialization hygiene for test cases that declare a dependency
# (`<local>.dep.kio`).
#
# A package reused as a dependency commits its materialized transitive
# closure: every consumer checks in the re-rooted `<local>/…` module tree
# its dependency materializes into. A clean checkout already carries the
# tree, so a consumer's fetch is self-contained and recursive fetch is
# never needed — and git content-addressing dedups the identical re-rooted
# blobs a shared dependency produces across consumers, so the committed
# closures cost little history. This gate enforces three invariants for
# every committed `<local>.dep.kio`:
#
#   1. The materialized tree is committed. The sibling `<local>/`
#      directory holds git-tracked files — the re-rooted module tree
#      `kio dep fetch` produces. (A `SKIP_DEP_MATERIALIZED` marker in the
#      case root exempts a case whose dependency is intentionally
#      non-materializable — a malformed-source fixture whose `kio dep
#      fetch` is expected to fail, so there is no tree to commit.)
#   2. No `.gitignore` hides the materialized tree. No committed
#      `.gitignore` under the case lists a `<local>/` line (the
#      pre-commit-closure policy ignored these trees; they are tracked
#      now). The repo-root `.gitignore` still owns `out/` and the build
#      cache; those are not materialized dependencies.
#   3. The committed tree is canonical. It matches what `kio dep fetch`
#      regenerates, with no drift (no stale, missing, or extra module).
#      The byte-level comparison runs in `--write` mode (which regenerates
#      every tree via the kio binary) and per-case under the test harness
#      (`ci/run-tests.sh` fetches before each build; the
#      `dep-canonical.sh` per-case check fails on any resulting drift).
#      The binary-free gate here asserts presence and tracking; canonical
#      content is the binary-driven half.
#
# Fix: rerun with `--write` to regenerate every committed tree via
# `kio dep fetch`, then commit the result; `git rm` any stale `.gitignore`
# that still hides a `<local>/` tree.
#
# `--write` locates the kio binary at `$KIO_BIN`, else
# `kio-rs/target/debug/kio`; build it first (`sh ../ci/cargo.sh build --bin
# kio` from `kio-rs/`) when neither exists.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
# shellcheck disable=SC1091 # absolute path derived from this script
. "$SCRIPT_DIR/../../infra/dep-materialization-order.sh"
cd "$REPO_ROOT"

WRITE=0
[ "${1:-}" = "--write" ] && WRITE=1

# Snapshot the committed `test-data/` file list once, from the commit
# object (`git ls-tree -r HEAD`), not the index (`git ls-files`). This
# gate runs alongside the per-case checks under `ci/all.sh`, and those run
# `git status` / `git check-ignore` against the same worktree; reading the
# committed tree object is immune to the index being concurrently
# refreshed, whereas a per-path `git ls-files` can race a sibling index
# rewrite and momentarily report a committed tree as absent. One `ls-tree`
# snapshot drives both the dependency-case discovery and every
# committed-presence test below.
tracked_snapshot=$(git ls-tree -r --name-only HEAD -- test-data/ 2>/dev/null || true)

# Does HEAD track at least one file under the given path prefix? Matches
# anchored at line start against the snapshot; the prefix is escaped so a
# `.` in a case name is literal.
tracked_under() {
  # Plain `grep` consuming all input, not `grep -q`: `-q` closes the pipe
  # on its first match, so the still-writing `printf` of the multi-KB
  # snapshot gets EPIPE — silent under a default SIGPIPE, but reported as
  # `printf: I/O error` under a CI parent that leaves SIGPIPE ignored.
  # Discarding stdout keeps the exit status (0 match / 1 miss) as the
  # function's result while leaving any real grep error on stderr.
  printf '%s\n' "$tracked_snapshot" \
    | grep "^$(printf '%s' "$1" | sed 's/[].[*^$\\/]/\\&/g')" >/dev/null
}

# Dependency-bearing cases live under test-data/ (goldens, POCs, castles).
# A `*.dep.kio` elsewhere — e.g. a vscode-extension syntax fixture — is a
# static sample that is never materialized, so it is out of scope (the
# snapshot is already scoped to test-data/).
dirs=$(printf '%s\n' "$tracked_snapshot" | grep '\.dep\.kio$' | sed 's|/[^/]*$||' | sort -u || true)

# A case opts out of the committed-tree requirement with a
# `SKIP_DEP_MATERIALIZED` marker — used only when the dependency is
# intentionally non-materializable (a malformed-source fixture whose
# `kio dep fetch` is expected to fail). The marker may sit in the case
# root or the workdir.
dep_skips() {
  ds_dir=$1
  case "$ds_dir" in
    */workdir/*) ds_root=${ds_dir%%/workdir/*} ;;
    */workdir) ds_root=${ds_dir%/workdir} ;;
    *) ds_root=$ds_dir ;;
  esac
  [ -f "$ds_root/SKIP_DEP_MATERIALIZED" ] && return 0
  [ -f "$ds_dir/SKIP_DEP_MATERIALIZED" ] && return 0
  return 1
}

# --write: regenerate every committed tree canonically via the kio binary,
# so the working tree holds exactly what `kio dep fetch` produces. The
# caller commits the result (and removes any stale tree-hiding .gitignore).
if [ "$WRITE" = 1 ]; then
  kio_bin=${KIO_BIN:-"$REPO_ROOT/kio-rs/target/debug/kio"}
  if [ ! -x "$kio_bin" ]; then
    printf 'dep-materialization: kio binary not found at %s\n' "$kio_bin" >&2
    printf '  build it first: (cd kio-rs && sh ../ci/cargo.sh build --bin kio)\n' >&2
    # shellcheck disable=SC2016 # $KIO_BIN is literal in the user message
    printf '  or point $KIO_BIN at an existing binary\n' >&2
    exit 2
  fi
  # A committed path dependency may itself be another dependency-bearing
  # package in test-data. Materialize those producers before their consumers
  # so each copied tree observes the producer's freshly regenerated closure.
  # Unknown and git sources impose no local ordering edge. Repeatedly select
  # the lexically first nodes whose local producers have already been removed;
  # no-progress means the committed fixture graph contains a cycle.
  write_dirs=
  for dir in $dirs; do
    dep_skips "$dir" && continue
    write_dirs="$write_dirs${write_dirs:+
}$dir"
  done
  ordered=$(dep_materialization_order "$write_dirs") || exit 1

  written=0
  for dir in $ordered; do
    # `--force`: `kio dep fetch` skips a dependency whose tree already
    # matches the lock, so a plain fetch would not regenerate an already-
    # committed tree. `--write` exists to rewrite every tree canonically
    # from source, so it bypasses the skip.
    if ( cd "$dir" && "$kio_bin" dep fetch --force >/dev/null 2>&1 ); then
      :
    else
      rc=$?
      # shellcheck disable=SC2016 # backticks are literal in the user message
      printf 'dep-materialization: `kio dep fetch --force` failed in %s\n' "$dir" >&2
      exit "$rc"
    fi
    written=$((written + 1))
  done
  printf 'dep-materialization: regenerated %s committed tree(s); commit the result\n' "$written"
  exit 0
fi

# Gating mode (binary-free): presence, tracking, and no tree-hiding
# .gitignore.
fail=0

# (1+2) Per declared dependency: the materialized tree is committed, and
# no committed .gitignore hides it.
for dir in $dirs; do
  gi="$dir/.gitignore"
  gi_tracked=0
  tracked_under "$gi" && gi_tracked=1

  for d in "$dir"/*.dep.kio; do
    local_name=$(basename "$d" .dep.kio)

    if [ "$gi_tracked" = 1 ] && grep -qxF "$local_name/" "$gi" 2>/dev/null; then
      # shellcheck disable=SC2016 # backticks are literal in the user message
      printf 'error: %s still ignores the materialized tree `%s/`; it is committed now — remove the line (or `git rm` the file if it only held tree lines)\n' \
        "$gi" "$local_name" >&2
      fail=1
    fi

    dep_skips "$dir" && continue
    if ! tracked_under "$dir/$local_name/"; then
      printf 'error: no committed materialized tree under %s/%s/\n' "$dir" "$local_name" >&2
      printf '  regenerate and commit it: rerun this check with --write, then commit\n' >&2
      printf '  (if the dependency is intentionally non-materializable, add a\n' >&2
      printf '   SKIP_DEP_MATERIALIZED marker in the case root)\n' >&2
      fail=1
    fi
  done
done

[ "$fail" = 0 ] || exit 1
printf 'dep-materialization: ok (%s dependency-bearing workdir(s))\n' \
  "$(printf '%s\n' "$dirs" | grep -c .)"
