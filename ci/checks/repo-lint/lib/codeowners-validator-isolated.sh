#!/bin/sh
#
# Run codeowners-validator against a detached snapshot of a clean checkout.
#
# The validator's experimental notowned check resets its repository worktree
# while enumerating tracked paths. That is safe only in a disposable worktree:
# ci/all.sh runs repo-lint jobs alongside checks that continuously read the
# active checkout.
#
# POSIX sh only.

set -eu

if [ "$#" -ne 2 ]; then
  printf 'usage: %s <repo-root> <codeowners-validator>\n' "$0" >&2
  exit 2
fi

REPO_ROOT=$1
VALIDATOR=$2

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/codeowners-validator.XXXXXX") || {
  printf 'codeowners-validator-isolated: cannot make scratch directory\n' >&2
  exit 2
}
snapshot=$scratch/worktree

cleanup() {
  trap - EXIT INT TERM HUP
  git -C "$REPO_ROOT" worktree remove --force "$snapshot" >/dev/null 2>&1 || :
  rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

if ! git -C "$REPO_ROOT" worktree add --quiet --detach "$snapshot" HEAD; then
  printf 'codeowners-validator-isolated: cannot create detached snapshot\n' >&2
  exit 2
fi

# Set both the documented repository variable and the current directory. The
# latter also confines validator subprocesses that invoke Git without passing
# REPOSITORY_PATH through to it.
(
  cd "$snapshot"
  REPOSITORY_PATH=$snapshot "$VALIDATOR"
)
