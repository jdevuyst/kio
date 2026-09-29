#!/bin/sh
# Git-sourced dependency, end to end, HERMETIC and DETERMINISTIC.
#
# A custom run.sh is justified here: the standard run.args path cannot
# assemble a git repository, and the consumer's `.dep.kio` URL points at
# a per-run scratch repository. This script (a) builds a bare git repo from the
# committed `library/greetlib` fixture using FIXED commit metadata so the
# resolved commit SHA is reproducible run-to-run and machine-to-machine,
# (b) writes the consumer's `greetlib.dep.kio` pointing at that repo with
# `ref "main"`, (c) builds + runs the consumer asserting it threads its
# host `String` through the re-rooted git dependency, and (d) verifies
# the produced `greetlib.lock.kio` pins the commit `ref "main"` resolved
# to. The git fetch is offline; `KIO_CACHE_HOME` is redirected into the
# scratch tree so the clone cache is isolated.
#
# `IS_KIO_PRIME` is a syntax-only assertion that the committed modules
# are Kio'-grammatical. `SKIP_KIO_PRIME_RUN` excludes direct Prime
# semantic execution because the natural surface source relies on
# universal-argument inference. The regular full-surface run remains covered.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

command -v git >/dev/null 2>&1 || fail "git is required for this test"

scratch=$(mktemp -d) || fail "cannot make scratch dir"
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

# Isolate the git-clone cache so the test never touches the user's real
# `$KIO_CACHE_HOME` / `$XDG_CACHE_HOME` and is reproducible.
KIO_CACHE_HOME="$scratch/kio-cache"
export KIO_CACHE_HOME
unset XDG_CACHE_HOME

# Copy the consumer package and the dependency-library fixture into the
# scratch tree (the build mutates both: it writes `out/`, materializes
# the dependency under `greetlib/`, and writes the lockfile).
cp -R workdir "$scratch/consumer" || fail "cannot copy consumer"
cp -R library "$scratch/library" || fail "cannot copy library fixture"

# --- Assemble the dependency as a bare git repo, with pinned metadata so
# the commit SHA is deterministic. ---
src="$scratch/library/greetlib"
(
  cd "$src" || exit 1
  git init --quiet -b main . || exit 1
  git add -A || exit 1
  # Pin every input to the commit hash: identity, message, and both the
  # author and committer timestamps. With a fixed tree and these fixed,
  # the SHA-1 commit id is fully determined.
  GIT_AUTHOR_NAME='Kio Test' \
  GIT_AUTHOR_EMAIL='test@kio.invalid' \
  GIT_AUTHOR_DATE='2020-01-01T00:00:00 +0000' \
  GIT_COMMITTER_NAME='Kio Test' \
  GIT_COMMITTER_EMAIL='test@kio.invalid' \
  GIT_COMMITTER_DATE='2020-01-01T00:00:00 +0000' \
    git commit --quiet -m 'fixture' || exit 1
) || fail "cannot assemble fixture git repo"

# Convert the work-tree repo to a bare repo the consumer clones from.
bare="$scratch/greetlib.git"
git clone --quiet --bare "$src" "$bare" || fail "cannot make bare repo"

# The deterministic resolved commit. We assert the lockfile records this
# exact SHA after `ref "main"` resolution.
expected_sha=$(git --git-dir="$bare" rev-parse HEAD) || fail "cannot read HEAD"

# --- Write the consumer's git `.dep.kio` (its URL is dynamic, hence
# generated here rather than committed). ---
cat > "$scratch/consumer/greetlib.dep.kio" <<EOF
dependency greetlib;

source {
  git "$bare";
  ref "main";
}
EOF

cd "$scratch/consumer" || fail "cannot enter consumer"

# Materialize the git dependency before building. Build / check / test
# consume a committed materialized tree as ordinary source and never
# materialize on their own; here the consumer's `.dep.kio` is generated
# (its URL is dynamic) rather than committed, so no tree is on disk and
# the harness's pre-build fetch never saw it. Fetch explicitly to
# materialize the re-rooted module the build then consumes.
"$KIO_BIN" dep fetch >fetch.out 2>fetch.err || {
  printf 'dep fetch failed:\n' >&2
  cat fetch.err >&2
  exit 1
}

# --- Build + run for the harness's target, asserting the consumer uses
# the dependency. ---
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>build.err || {
  printf 'build failed:\n' >&2
  cat build.err >&2
  exit 1
}

# The lockfile must exist and pin the resolved commit. (It is the
# committed pin; here we only assert its content is correct.)
lock="greetlib.lock.kio"
[ -f "$lock" ] || fail "expected lockfile $lock was not produced"
grep -q "commit \"$expected_sha\";" "$lock" \
  || fail "lockfile did not record the resolved commit $expected_sha; lockfile:
$(cat "$lock")"
grep -q 'ref "main";' "$lock" || fail "lockfile did not record ref \"main\""

# Run the built package; its stdout is the golden's expected.stdout.
exec "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET"
