#!/bin/sh
# `kio dep update` re-pins a git dependency A -> B, HERMETIC + DETERMINISTIC.
#
# Subject: `kio dep update` re-resolves a git dependency's `ref` to the
# commit it designates *now* and rewrites the committed `<local>.lock.kio`,
# ignoring the commit the lock pinned before. This script:
#
#   1. Assembles the dependency repo at commit A (the committed
#      `library/greetlib` fixture) and pins the consumer's lockfile to A
#      via `kio dep fetch` (the realistic "already locked" starting state).
#   2. Advances the dependency's `main` to a *distinct* commit B — a
#      different tree (its `marker` returns the other argument), hence a
#      distinct, reproducible SHA. Both A and B use FIXED commit metadata
#      so both SHAs are deterministic run-to-run and machine-to-machine.
#   3. Runs `kio dep update` and asserts the lockfile's commit moved from
#      A to B (the old commit is gone, the new commit B is recorded) and
#      that the command reported the `old -> new` move.
#   4. Builds + runs the consumer, asserting it now threads its host
#      `String` through *commit B's* `marker` (which prints the second
#      argument) — so the run proves the re-pin changed which commit's
#      code is used, not merely that the lockfile SHA string changed.
#
# A custom run.sh is justified: the standard run.args path cannot
# assemble a git repository or advance it across two commits, and the
# consumer's `.dep.kio` URL points at a per-run scratch repository. The
# git operations are offline; `KIO_CACHE_HOME` is redirected into the
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

KIO_CACHE_HOME="$scratch/kio-cache"
export KIO_CACHE_HOME
unset XDG_CACHE_HOME

cp -R workdir "$scratch/consumer" || fail "cannot copy consumer"
cp -R library "$scratch/library" || fail "cannot copy library fixture"

# Pinned author + committer identity, shared by both commits. Only the
# tree and the per-commit date differ, so each SHA is fully determined.
GIT_AUTHOR_NAME='Kio Test'
GIT_AUTHOR_EMAIL='test@kio.invalid'
GIT_COMMITTER_NAME='Kio Test'
GIT_COMMITTER_EMAIL='test@kio.invalid'
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL

src="$scratch/library/greetlib"

# --- Commit A: the committed fixture, `marker` returns its first arg. ---
(
  cd "$src" || exit 1
  git init --quiet -b main . || exit 1
  git add -A || exit 1
  GIT_AUTHOR_DATE='2020-01-01T00:00:00 +0000' \
  GIT_COMMITTER_DATE='2020-01-01T00:00:00 +0000' \
    git commit --quiet -m 'commit A' || exit 1
) || fail "cannot assemble commit A"

bare="$scratch/greetlib.git"
git clone --quiet --bare "$src" "$bare" || fail "cannot make bare repo"
sha_a=$(git --git-dir="$bare" rev-parse HEAD) || fail "cannot read commit A"

# --- Write the consumer's git `.dep.kio` and pin it to commit A by
# fetching once. This is the realistic "lockfile already pins the old
# commit" starting state `dep update` exists to advance. ---
cat > "$scratch/consumer/greetlib.dep.kio" <<EOF
dependency greetlib;

source {
  git "$bare";
  ref "main";
}
EOF

cd "$scratch/consumer" || fail "cannot enter consumer"

"$KIO_BIN" dep fetch >/dev/null 2>fetch.err || {
  printf 'initial dep fetch failed:\n' >&2
  cat fetch.err >&2
  exit 1
}
lock="greetlib.lock.kio"
grep -q "commit \"$sha_a\";" "$lock" \
  || fail "lockfile was not pinned to commit A ($sha_a); lockfile:
$(cat "$lock")"

# --- Commit B: change `marker` to return its SECOND argument. A distinct
# tree, so a distinct SHA; FIXED metadata, so B is reproducible. ---
cat > "$src/greet.kio" <<'EOF'
module greet;

// Commit-B behavior: return the SECOND of two like-typed values.
pub fn marker[A](_before: A, after: A) -> A { after }
EOF
(
  cd "$src" || exit 1
  git add -A || exit 1
  GIT_AUTHOR_DATE='2020-01-02T00:00:00 +0000' \
  GIT_COMMITTER_DATE='2020-01-02T00:00:00 +0000' \
    git commit --quiet -m 'commit B' || exit 1
  # Publish the advanced `main` into the bare repo the consumer resolves.
  git push --quiet "$bare" main || exit 1
) || fail "cannot assemble + publish commit B"
sha_b=$(git --git-dir="$bare" rev-parse HEAD) || fail "cannot read commit B"

[ "$sha_a" != "$sha_b" ] \
  || fail "commits A and B unexpectedly share a SHA ($sha_a) — the trees must differ"

# --- Subject: re-pin the lockfile from A to B. ---
"$KIO_BIN" dep update >update.out 2>update.err || {
  printf 'dep update failed:\n' >&2
  cat update.err >&2
  exit 1
}

# The command reports the A -> B move (commits abbreviated to 12 chars).
short_a=$(printf '%s' "$sha_a" | cut -c1-12)
short_b=$(printf '%s' "$sha_b" | cut -c1-12)
grep -q "\`greetlib\` (git): $short_a -> $short_b" update.out \
  || fail "dep update did not report the $short_a -> $short_b move; output:
$(cat update.out)"

# The lockfile now pins B, and no longer pins A.
grep -q "commit \"$sha_b\";" "$lock" \
  || fail "lockfile was not re-pinned to commit B ($sha_b); lockfile:
$(cat "$lock")"
grep -q "commit \"$sha_a\";" "$lock" \
  && fail "lockfile still pins the old commit A ($sha_a) after update; lockfile:
$(cat "$lock")"

# --- Build + run, asserting the consumer now uses commit B's `marker`
# (which prints the second argument). The re-pin changed which commit's
# code is materialized + built, not just the lockfile string. ---
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>build.err || {
  printf 'build failed:\n' >&2
  cat build.err >&2
  exit 1
}

exec "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET"
