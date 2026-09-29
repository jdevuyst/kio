#!/bin/sh
# `kio dep update` across a COMPATIBLE dependency-contract change,
# HERMETIC + DETERMINISTIC.
#
# Subject: `kio dep update`'s contract-honesty gate. Before re-pinning an
# A -> B move it compares the dependency's contract surface at the old
# commit against the new commit's. A *compatible* move (here: commit B
# adds a `third` export and seals it as v2) re-pins silently — exit 0,
# the lock's `commit` and `sig` advance, and no warning is printed.
#
# This script:
#   1. Assembles the dependency repo at commit A (exports `first` +
#      `second`, with a sealed v1 `greetlib.sig.kio`) and pins the
#      consumer's lockfile to A via `kio dep fetch` (recording A's
#      contract digest in the lock's `sig`).
#   2. Advances the dependency's `main` to commit B — a compatible
#      superset (`first` + `second` + `third`) with a sealed v2 sig.
#      FIXED commit metadata keeps both SHAs reproducible.
#   3. Runs `kio dep update` and asserts it succeeded (exit 0), re-pinned
#      A -> B, advanced the recorded `sig` digest, and printed no gate
#      warning.
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

GIT_AUTHOR_NAME='Kio Test'
GIT_AUTHOR_EMAIL='test@kio.invalid'
GIT_COMMITTER_NAME='Kio Test'
GIT_COMMITTER_EMAIL='test@kio.invalid'
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL

src="$scratch/library/greetlib"

# --- Commit A: the sealed v1 contract (first + second). ---
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

# --- Pin the consumer to commit A. ---
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
grep -q "commit \"$sha_a\";" "$lock" || fail "lockfile not pinned to A; lockfile:
$(cat "$lock")"
sig_a=$(grep 'sig ' "$lock") || fail "lockfile recorded no sig at A"

# --- Commit B: compatible superset (add `third`), sealed v2. ---
cp "$scratch/library/commitB/greet.kio" "$src/greet.kio" || fail "cannot apply B greet"
cp "$scratch/library/commitB/greetlib.sig.kio" "$src/greetlib.sig.kio" || fail "cannot apply B sig"
(
  cd "$src" || exit 1
  git add -A || exit 1
  GIT_AUTHOR_DATE='2020-01-02T00:00:00 +0000' \
  GIT_COMMITTER_DATE='2020-01-02T00:00:00 +0000' \
    git commit --quiet -m 'commit B' || exit 1
  git push --quiet "$bare" main || exit 1
) || fail "cannot assemble + publish commit B"
sha_b=$(git --git-dir="$bare" rev-parse HEAD) || fail "cannot read commit B"
[ "$sha_a" != "$sha_b" ] || fail "commits A and B unexpectedly share a SHA"

# --- Subject: a compatible re-pin proceeds silently. ---
"$KIO_BIN" dep update >update.out 2>update.err
rc=$?
[ "$rc" -eq 0 ] || fail "dep update should succeed on a compatible move (exit $rc); stderr:
$(cat update.err)"

# The gate emits no warning on a compatible move.
if grep -q 'breaking' update.err; then
  fail "compatible update unexpectedly warned about a breaking change:
$(cat update.err)"
fi

# The command reported the A -> B move.
short_a=$(printf '%s' "$sha_a" | cut -c1-12)
short_b=$(printf '%s' "$sha_b" | cut -c1-12)
grep -q "\`greetlib\` (git): $short_a -> $short_b" update.out \
  || fail "dep update did not report the $short_a -> $short_b move; output:
$(cat update.out)"

# The lock advanced to B, and its recorded contract digest changed.
grep -q "commit \"$sha_b\";" "$lock" || fail "lockfile not re-pinned to B; lockfile:
$(cat "$lock")"
sig_b=$(grep 'sig ' "$lock")
[ "$sig_a" != "$sig_b" ] \
  || fail "recorded sig digest did not change across the compatible re-pin ($sig_a)"

# --- Build + run the consumer against the re-pinned dependency, to
# confirm the new commit materializes and the consumer links against it.
# Its stdout is the golden's expected.stdout. ---
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>build.err || {
  printf 'build failed after re-pin:\n' >&2
  cat build.err >&2
  exit 1
}
exec "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET"
