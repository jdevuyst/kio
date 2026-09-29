#!/bin/sh
# `kio dep fetch` against a git dependency, HERMETIC and DETERMINISTIC.
#
# Subject: `kio dep fetch` runs the dependency materialization a build
# performs implicitly, but on its own and *without* a build — it fetches
# the git dependency into the cache, resolves its `ref` to a commit,
# writes the committed `<local>.lock.kio` pin, checks the commit out, and
# re-roots the dependency's modules under `greetlib/`. This script
# asserts `dep fetch` (a) produces the lockfile pinning the deterministic
# resolved commit and (b) materializes the re-rooted module on disk —
# *before* any `kio build` runs. It then builds + runs the consumer to
# confirm the fetched-and-re-rooted modules are consumable.
#
# A custom run.sh is justified: the standard run.args path cannot
# assemble a git repository, and the consumer's `.dep.kio` URL points at
# a per-run scratch repository. The git fetch is offline, and
# `KIO_CACHE_HOME` is redirected into the scratch tree so the clone
# cache is isolated and never touches the user's real cache.
#
# The dependency is built from the committed `library/greetlib` fixture
# with FIXED commit metadata (identity + both timestamps), so the
# resolved commit SHA is reproducible run-to-run and machine-to-machine
# and the lockfile assertion is exact.
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
# scratch tree (the commands mutate both: `dep fetch` materializes the
# dependency under `greetlib/` and writes the lockfile; the build writes
# `out/`).
cp -R workdir "$scratch/consumer" || fail "cannot copy consumer"
cp -R library "$scratch/library" || fail "cannot copy library fixture"

# --- Assemble the dependency as a bare git repo, with pinned metadata so
# the commit SHA is deterministic. ---
src="$scratch/library/greetlib"
(
  cd "$src" || exit 1
  git init --quiet -b main . || exit 1
  git add -A || exit 1
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

# --- Subject: fetch the dependency WITHOUT building. ---
"$KIO_BIN" dep fetch >fetch.out 2>fetch.err || {
  printf 'dep fetch failed:\n' >&2
  cat fetch.err >&2
  exit 1
}

# `dep fetch` reports what it materialized.
grep -q "fetched \`greetlib\` (git)" fetch.out \
  || fail "dep fetch did not report fetching greetlib; output:
$(cat fetch.out)"

# The lockfile must exist and pin the deterministic resolved commit —
# produced by `dep fetch` alone, before any build.
lock="greetlib.lock.kio"
[ -f "$lock" ] || fail "dep fetch did not produce the lockfile $lock"
grep -q "commit \"$expected_sha\";" "$lock" \
  || fail "lockfile did not record the resolved commit $expected_sha; lockfile:
$(cat "$lock")"
grep -q 'ref "main";' "$lock" || fail "lockfile did not record ref \"main\""

# The re-rooted dependency module must be on disk — `dep fetch`
# materialized it under the `greetlib/` prefix, before any build.
[ -f "greetlib/greet.kio" ] \
  || fail "dep fetch did not materialize the re-rooted module greetlib/greet.kio"

# --- Build + run, confirming the fetched-and-re-rooted modules are
# consumable by the consumer. The build reuses the already-materialized
# tree; nothing is re-fetched. ---
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>build.err || {
  printf 'build failed:\n' >&2
  cat build.err >&2
  exit 1
}

# Run the built package; its stdout is the golden's expected.stdout.
exec "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET"
