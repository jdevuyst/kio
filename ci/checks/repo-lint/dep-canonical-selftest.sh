#!/bin/sh
#
# Self-test for the per-case dependency-canonicality drift check
# (ci/checks/per-case/dep-canonical.sh).
#
# The drift check regenerates a committed materialized dependency tree with
# `kio dep fetch --force` and asserts the committed tree is byte-for-byte
# what fetch produces. This self-test pins the two properties a previous
# version of that check got wrong:
#
#   1. A `kio dep fetch` that FAILS is fatal — not swallowed. The old
#      check ran `... dep fetch ... || :`, discarding a non-zero exit, so a
#      fetch that failed before writing left the committed tree unchanged,
#      the `git status` comparison was empty, and the check falsely passed
#      (asserting "committed == committed" instead of "committed == fetch
#      output"). The check must now fail when the fetch fails.
#
#   2. A clean, drift-free tree still passes, and a genuinely drifted tree
#      still fails — so making the fetch fatal did not break the check's
#      normal verdicts.
#
# It drives dep-canonical.sh with a stub `kio` binary (no real build
# needed): the stub's behavior — fail, no-op, or rewrite-a-module — selects
# which scenario is exercised. Hermetic: everything lives in a `mktemp`
# scratch tree, and the scenarios that reach the `git status` comparison
# initialize their own throwaway git repository.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
DEP_CANONICAL="$REPO_ROOT/ci/checks/per-case/dep-canonical.sh"

if [ ! -f "$DEP_CANONICAL" ]; then
  printf 'dep-canonical-selftest: cannot find %s\n' "$DEP_CANONICAL" >&2
  exit 2
fi

scratch=$(mktemp -d) || {
  printf 'dep-canonical-selftest: cannot make scratch dir\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

fail=0

# Assemble a case directory ($1) holding a committed-style materialized
# dependency tree: `workdir/foo.dep.kio` plus its re-rooted `workdir/foo/`
# module. The named stub-mode file `$1/stub-mode` selects the stub `kio`
# behavior at fetch time.
make_case() {
  case_dir="$1"
  mkdir -p "$case_dir/workdir/foo"
  cat > "$case_dir/workdir/foo.dep.kio" <<'EOF'
dependency foo;

source {
  path "../library/foo/foo.pkg.kio";
}
EOF
  printf 'module foo/greet;\n\npub fn hi() -> . { () }\n' > "$case_dir/workdir/foo/greet.kio"
}

# A stub `kio` binary. dep-canonical.sh calls it as `kio dep fetch --force`
# from inside `workdir/`. The stub reads `$STUB_MODE`:
#   fail    — exit 1 (a fetch failure), writing nothing.
#   noop    — exit 0, writing nothing (the tree is already canonical).
#   drift   — exit 0, but rewrite the materialized module (a regeneration
#             that differs from the committed tree).
write_stub() {
  cat > "$scratch/kio" <<'EOF'
#!/bin/sh
case "${STUB_MODE:-noop}" in
  fail)  exit 1 ;;
  drift) printf 'module foo/greet;\n\npub fn hi() -> . { hi() }\n' > foo/greet.kio; exit 0 ;;
  *)     exit 0 ;;
esac
EOF
  chmod +x "$scratch/kio"
}

write_stub

# --- Scenario 1: a failing `kio dep fetch` is fatal. ---
# No git repo needed: a fatal fetch must abort before the git comparison.
case1="$scratch/case-fail"
make_case "$case1"
rc=0
( cd "$case1" && STUB_MODE=fail KIO_BIN="$scratch/kio" sh "$DEP_CANONICAL" ) \
  >/dev/null 2>&1 || rc=$?
if [ "$rc" -eq 0 ]; then
  # shellcheck disable=SC2016 # backticks are literal in the user message
  printf 'dep-canonical-selftest: FAIL — a failing `kio dep fetch` was not fatal (exit 0)\n' >&2
  fail=1
fi

# Scenarios 2 and 3 reach the `git status` comparison, so they run inside a
# throwaway git repo with the materialized tree committed.
repo="$scratch/repo"
mkdir -p "$repo"
(
  cd "$repo"
  git init --quiet -b main .
  git config user.email test@kio.invalid
  git config user.name 'Kio Test'
) || {
  printf 'dep-canonical-selftest: cannot init scratch git repo\n' >&2
  exit 2
}
make_case "$repo"
( cd "$repo" && git add -A && git commit --quiet -m 'committed tree' ) || {
  printf 'dep-canonical-selftest: cannot commit scratch tree\n' >&2
  exit 2
}

# --- Scenario 2: a clean, drift-free tree still passes. ---
rc=0
( cd "$repo" && STUB_MODE=noop KIO_BIN="$scratch/kio" sh "$DEP_CANONICAL" ) \
  >/dev/null 2>&1 || rc=$?
if [ "$rc" -ne 0 ]; then
  printf 'dep-canonical-selftest: FAIL — a drift-free tree was rejected (exit %s)\n' "$rc" >&2
  fail=1
fi

# --- Scenario 3: a genuinely drifted regeneration still fails. ---
rc=0
( cd "$repo" && STUB_MODE=drift KIO_BIN="$scratch/kio" sh "$DEP_CANONICAL" ) \
  >/dev/null 2>&1 || rc=$?
if [ "$rc" -eq 0 ]; then
  printf 'dep-canonical-selftest: FAIL — a drifted tree was not caught (exit 0)\n' >&2
  fail=1
fi
# Restore the committed tree the drift scenario rewrote (scratch is removed
# on exit anyway; this keeps the repo tidy if the trap is bypassed).
( cd "$repo" && git checkout --quiet -- workdir ) || :

if [ "$fail" -ne 0 ]; then
  exit 1
fi

printf 'dep-canonical-selftest: ok (fatal-on-failure, clean-passes, drift-fails)\n'
exit 0
