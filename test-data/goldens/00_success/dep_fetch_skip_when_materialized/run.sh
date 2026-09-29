#!/bin/sh
# `kio dep fetch` skips an already-materialized dependency, and `--force`
# re-materializes it. HERMETIC + DETERMINISTIC.
#
# Subject: a `kio dep fetch` whose dependency is already materialized at
# the lock's intent (its re-rooted tree on disk matches the locked commit
# byte-for-byte) is a no-op — it reports `up to date` and rewrites
# nothing, eliding the redundant re-fetch a build/check would otherwise
# repeat. `--force` bypasses the skip and re-materializes regardless. A
# drift (a hand-deleted materialized module) is *not* up to date and
# triggers a re-fetch. This script asserts each of those, then builds +
# runs the consumer to confirm the materialized tree is still consumable.
#
# A custom run.sh is justified: the standard run.args path cannot assemble
# a git repository, and the consumer's `.dep.kio` URL points at a per-run
# scratch repository. The git fetch is offline, and `KIO_CACHE_HOME` is
# redirected into the scratch tree so the clone cache is isolated and
# never touches the user's real cache. FIXED commit metadata makes the
# resolved commit reproducible, so the lock pin is exact run-to-run.
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

cp -R workdir "$scratch/consumer" || fail "cannot copy consumer"
cp -R library "$scratch/library" || fail "cannot copy library fixture"

# --- Assemble the dependency as a bare git repo, pinned metadata so the
# commit SHA is deterministic. ---
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

bare="$scratch/greetlib.git"
git clone --quiet --bare "$src" "$bare" || fail "cannot make bare repo"

cat > "$scratch/consumer/greetlib.dep.kio" <<EOF
dependency greetlib;

source {
  git "$bare";
  ref "main";
}
EOF

cd "$scratch/consumer" || fail "cannot enter consumer"

mat="greetlib/greet.kio"

# --- 1) First fetch materializes the dependency (reports `fetched`). ---
"$KIO_BIN" dep fetch >f1.out 2>f1.err || {
  printf 'first dep fetch failed:\n' >&2
  cat f1.err >&2
  exit 1
}
grep -q "fetched \`greetlib\` (git)" f1.out \
  || fail "first fetch did not report \`fetched\`; output:
$(cat f1.out)"
[ -f "$mat" ] || fail "first fetch did not materialize $mat"

# --- 2) Second fetch with nothing changed is the skip: `up to date`,
# not `fetched`. This is the redundant-fetch elision. ---
"$KIO_BIN" dep fetch >f2.out 2>f2.err || {
  printf 'second dep fetch failed:\n' >&2
  cat f2.err >&2
  exit 1
}
grep -q "\`greetlib\` (git): up to date" f2.out \
  || fail "second fetch did not report \`up to date\`; output:
$(cat f2.out)"
grep -q "fetched \`greetlib\`" f2.out \
  && fail "second fetch re-fetched an already-materialized dependency; output:
$(cat f2.out)"

# --- 3) `--force` re-materializes even when already current. ---
"$KIO_BIN" dep fetch --force >f3.out 2>f3.err || {
  printf 'forced dep fetch failed:\n' >&2
  cat f3.err >&2
  exit 1
}
grep -q "fetched \`greetlib\` (git)" f3.out \
  || fail "--force did not re-fetch; output:
$(cat f3.out)"

# --- 4) A drift (a hand-deleted materialized module) is not up to date:
# the next plain fetch re-materializes it. ---
rm -f "$mat" || fail "cannot delete materialized module $mat"
"$KIO_BIN" dep fetch >f4.out 2>f4.err || {
  printf 'post-drift dep fetch failed:\n' >&2
  cat f4.err >&2
  exit 1
}
grep -q "fetched \`greetlib\` (git)" f4.out \
  || fail "fetch after a drift did not re-materialize; output:
$(cat f4.out)"
[ -f "$mat" ] || fail "fetch after a drift did not rewrite $mat"

# --- Build + run, confirming the materialized tree is consumable. ---
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>build.err || {
  printf 'build failed:\n' >&2
  cat build.err >&2
  exit 1
}

exec "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET"
