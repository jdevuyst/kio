#!/bin/sh
#
# Prove the CODEOWNERS notowned check cannot mutate the checkout that
# ci/all.sh's other jobs read. The validator stub creates the same destructive
# window as the real checker: it removes tracked and untracked sentinels, waits
# while a concurrent reader observes the tree, then runs `git reset --hard`.
# The isolation helper must confine all of that to a detached worktree.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
HELPER="$SCRIPT_DIR/lib/codeowners-validator-isolated.sh"

if [ ! -f "$HELPER" ]; then
  printf 'github-lint-isolation-selftest: missing helper: %s\n' "$HELPER" >&2
  exit 2
fi

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/github-lint-isolation-selftest.XXXXXX") || {
  printf 'github-lint-isolation-selftest: cannot make scratch directory\n' >&2
  exit 2
}
reader_pid=
cleanup() {
  trap - EXIT INT TERM HUP
  if [ -n "$reader_pid" ]; then
    kill "$reader_pid" >/dev/null 2>&1 || :
    wait "$reader_pid" >/dev/null 2>&1 || :
  fi
  rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

active=$scratch/active
mkdir -p "$active"
(
  cd "$active"
  git init --quiet -b main .
  git config user.email test@kio.invalid
  git config user.name 'Kio Test'
  printf 'tracked and stable\n' >tracked-sentinel
  git add tracked-sentinel
  git commit --quiet -m 'fixture'
)
printf 'untracked and stable\n' >"$active/untracked-sentinel"

validator=$scratch/codeowners-validator
cat >"$validator" <<'EOF'
#!/bin/sh
set -eu
: "${REPOSITORY_PATH:?}"
: "${KIO_TEST_VALIDATOR_PATH_FILE:?}"
[ "${CHECKS:-}" = 'duppatterns,syntax' ]
[ "${EXPERIMENTAL_CHECKS:-}" = 'notowned,avoid-shadowing' ]
printf '%s\n' "$REPOSITORY_PATH" >"$KIO_TEST_VALIDATOR_PATH_FILE"
rm -f "$REPOSITORY_PATH/tracked-sentinel" \
  "$REPOSITORY_PATH/untracked-sentinel"
sleep 1
git -C "$REPOSITORY_PATH" reset --hard HEAD >/dev/null
EOF
chmod +x "$validator"

# This direct helper deliberately removes the isolation boundary. The opt-in
# demonstrates that this regression test goes red without the detached
# worktree.
direct_helper=$scratch/direct-helper
cat >"$direct_helper" <<'EOF'
#!/bin/sh
set -eu
repo=$1
validator=$2
(
  cd "$repo"
  REPOSITORY_PATH=$repo "$validator"
)
EOF
chmod +x "$direct_helper"
if [ "${KIO_TEST_CODEOWNERS_DIRECT:-}" = 1 ]; then
  HELPER=$direct_helper
fi

reader_done=$scratch/reader-done
reader_failed=$scratch/reader-failed
reader_ready=$scratch/reader-ready
(
  : >"$reader_ready"
  while [ ! -e "$reader_done" ]; do
    tracked=
    untracked=
    if [ -r "$active/tracked-sentinel" ]; then
      IFS= read -r tracked <"$active/tracked-sentinel" || :
    fi
    if [ -r "$active/untracked-sentinel" ]; then
      IFS= read -r untracked <"$active/untracked-sentinel" || :
    fi
    if [ "$tracked" != 'tracked and stable' ] \
      || [ "$untracked" != 'untracked and stable' ]; then
      : >"$reader_failed"
      exit 0
    fi
  done
) &
reader_pid=$!
while [ ! -e "$reader_ready" ]; do :; done

validator_path=$scratch/validator-path
isolation_tmp=$scratch/isolation-tmp
mkdir -p "$isolation_tmp"
rc=0
CHECKS=duppatterns,syntax \
  EXPERIMENTAL_CHECKS=notowned,avoid-shadowing \
  KIO_TEST_VALIDATOR_PATH_FILE=$validator_path \
  TMPDIR=$isolation_tmp \
  sh "$HELPER" "$active" "$validator" || rc=$?
: >"$reader_done"
wait "$reader_pid" || :
reader_pid=

fail=0
if [ "$rc" -ne 0 ]; then
  printf 'github-lint-isolation-selftest: helper exited %s\n' "$rc" >&2
  fail=1
fi
if [ -e "$reader_failed" ]; then
  printf 'github-lint-isolation-selftest: concurrent reader observed a missing or changed active sentinel\n' >&2
  fail=1
fi
if [ "$(cat "$active/tracked-sentinel" 2>/dev/null || :)" != 'tracked and stable' ]; then
  printf 'github-lint-isolation-selftest: active tracked sentinel changed\n' >&2
  fail=1
fi
if [ "$(cat "$active/untracked-sentinel" 2>/dev/null || :)" != 'untracked and stable' ]; then
  printf 'github-lint-isolation-selftest: active untracked sentinel changed\n' >&2
  fail=1
fi
if [ ! -s "$validator_path" ]; then
  printf 'github-lint-isolation-selftest: validator did not run\n' >&2
  fail=1
else
  invoked_root=$(cat "$validator_path")
  if [ "$invoked_root" = "$active" ]; then
    printf 'github-lint-isolation-selftest: validator ran in the active checkout\n' >&2
    fail=1
  fi
  if [ -e "$invoked_root" ]; then
    printf 'github-lint-isolation-selftest: disposable validator worktree was not removed\n' >&2
    fail=1
  fi
fi

[ "$fail" -eq 0 ] || exit 1
printf 'github-lint-isolation-selftest: ok (destructive validator confined to disposable worktree)\n'
