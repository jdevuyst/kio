#!/bin/sh
# `kio dep update` BLOCKED by a breaking change to a SEALED dependency
# contract, then forced with `--allow-breaking`. HERMETIC + DETERMINISTIC.
#
# Subject: `kio dep update`'s contract-honesty gate on a *sealed*
# dependency contract. When re-pinning A -> B would adopt a breaking
# change (here: commit B drops the sealed `second` export and seals the
# removal as a breaking v2) and the dependency's contract is sealed, the
# re-pin is a dependency error (exit 30) and the lock is left unchanged —
# the consumer stays on the reproducible old commit. `--allow-breaking`
# downgrades the error to a warning and proceeds.
#
# This script:
#   1. Assembles the dependency at commit A (sealed v1: `first` +
#      `second`) and pins the consumer's lockfile to A.
#   2. Advances `main` to commit B — a sealed v2 that *removes* the
#      `second` export (a breaking change). FIXED commit metadata keeps
#      both SHAs reproducible.
#   3. Asserts `kio dep update` (no flag) errors with exit 30, names the
#      breaking change, and leaves the lock pinned to A (commit + sig
#      unchanged).
#   4. Asserts `kio dep update --allow-breaking` succeeds (exit 0), warns
#      about the adopted break on stderr, and re-pins the lock to B.
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

# --- Commit A: sealed v1 (first + second). ---
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

# --- Commit B: sealed v2 dropping the `second` export (breaking). ---
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

# --- Subject part 1: the gate BLOCKS the breaking re-pin (exit 30). ---
"$KIO_BIN" dep update >block.out 2>block.err
rc=$?
[ "$rc" -eq 30 ] \
  || fail "sealed-breaking dep update should error with exit 30, got $rc; stderr:
$(cat block.err)"
grep -q 'greet.second' block.err \
  || fail "blocked update did not name the breaking change; stderr:
$(cat block.err)"
grep -q -- '--allow-breaking' block.err \
  || fail "blocked update did not suggest --allow-breaking; stderr:
$(cat block.err)"

# The lock is untouched: still pinned to A, same recorded digest.
grep -q "commit \"$sha_a\";" "$lock" \
  || fail "blocked update must leave the lock pinned to A; lockfile:
$(cat "$lock")"
[ "$sig_a" = "$(grep 'sig ' "$lock")" ] \
  || fail "blocked update must not rewrite the recorded sig digest"

# --- Subject part 2: --allow-breaking adopts the break (exit 0 + warn). ---
"$KIO_BIN" dep update --allow-breaking >allow.out 2>allow.err
rc=$?
[ "$rc" -eq 0 ] \
  || fail "dep update --allow-breaking should succeed (exit $rc); stderr:
$(cat allow.err)"
grep -q 'warning' allow.err \
  || fail "--allow-breaking should warn about the adopted break; stderr:
$(cat allow.err)"
grep -q 'greet.second' allow.err \
  || fail "--allow-breaking warning should name the breaking change; stderr:
$(cat allow.err)"

# The lock now advanced to B, with a changed contract digest.
grep -q "commit \"$sha_b\";" "$lock" \
  || fail "--allow-breaking did not re-pin to B; lockfile:
$(cat "$lock")"
[ "$sig_a" != "$(grep 'sig ' "$lock")" ] \
  || fail "--allow-breaking did not advance the recorded sig digest"

# --- Build + run the consumer against the re-pinned dependency, to
# confirm commit B materializes and the consumer links against it (it
# uses only `first`, which survives the breaking removal of `second`).
# Its stdout is the golden's expected.stdout. ---
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>build.err || {
  printf 'build failed after forced re-pin:\n' >&2
  cat build.err >&2
  exit 1
}
exec "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET"
