#!/bin/sh
# `kio dep update` is ATOMIC across multiple dependencies: when one
# dependency's honesty gate blocks a sealed break, NO dependency's lock is
# advanced — not even a compatible one processed earlier in the run.
# HERMETIC + DETERMINISTIC.
#
# Subject: two git dependencies are re-pinned in one `kio dep update`.
# `alib` (sorts first, so it is processed first) moves A -> B compatibly;
# `blib` moves A -> B with a breaking change to its *sealed* contract
# (it drops the `second` export, sealed as v2). Both packages are selected
# explicitly from nested directories: old and new sealed contracts must come
# from those selected manifests. The breaking move blocks the whole command
# (exit 30). The fix under test stages every re-pin and
# commits the locks only after all gates pass, so the block leaves *both*
# locks pinned to A — `alib`'s included. Before the fix, `alib`'s lock was
# written the moment its own gate passed, so it advanced to B while its
# committed tree still reflected A: a partial, inconsistent state.
#
# The script:
#   1. Assembles `alib` and `blib`, each at commit A, and pins the
#      consumer's two lockfiles to A.
#   2. Advances both repos to commit B — `alib` compatibly (a comment-only
#      change), `blib` breakingly (drop `second`, sealed v2). FIXED commit
#      metadata keeps every SHA reproducible.
#   3. Asserts a plain `kio dep update` errors (exit 30) AND leaves BOTH
#      locks pinned to A — the atomicity invariant.
#   4. Asserts `kio dep update --allow-breaking` then advances both to B
#      (exit 0), and builds + runs the consumer against the re-pinned
#      dependencies to confirm the healed tree is consumable.
#
# A custom run.sh is justified: the standard run.args path cannot assemble
# two git repositories or advance each across two commits, and the
# consumer's `.dep.kio` URLs point at per-run scratch repositories. The git
# operations are offline; `KIO_CACHE_HOME` is redirected into the scratch
# tree so the clone cache is isolated.
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

# Assemble one dependency repo ($1=src dir) at commit A and clone a bare
# mirror ($2). Prints the commit-A SHA on stdout.
init_repo() {
  _src="$1"
  _bare="$2"
  (
    cd "$_src" || exit 1
    git init --quiet -b main . || exit 1
    git add -A || exit 1
    GIT_AUTHOR_DATE='2020-01-01T00:00:00 +0000' \
    GIT_COMMITTER_DATE='2020-01-01T00:00:00 +0000' \
      git commit --quiet -m 'commit A' || exit 1
  ) || return 1
  git clone --quiet --bare "$_src" "$_bare" || return 1
  git --git-dir="$_bare" rev-parse HEAD
}

alib_repo="$scratch/alib-repository"
blib_repo="$scratch/blib-repository"
mkdir -p "$alib_repo/packages/deep" "$blib_repo/packages/deep" || fail "create nested roots"
cp -R "$scratch/library/alib" "$alib_repo/packages/deep/lib" || fail "nest alib"
cp -R "$scratch/library/blib" "$blib_repo/packages/deep/lib" || fail "nest blib"
alib_src="$alib_repo/packages/deep/lib"
blib_src="$blib_repo/packages/deep/lib"
alib_bare="$scratch/alib.git"
blib_bare="$scratch/blib.git"

sha_a_alib=$(init_repo "$alib_repo" "$alib_bare") || fail "cannot assemble alib commit A"
sha_a_blib=$(init_repo "$blib_repo" "$blib_bare") || fail "cannot assemble blib commit A"

# Two dependency declarations. `alib` sorts before `blib`, so the update
# loop processes `alib` (the compatible move) first and `blib` (the block)
# second — the order that exposes a non-atomic re-pin.
cat > "$scratch/consumer/alib.dep.kio" <<EOF
dependency alib;

source {
  git "$alib_bare";
  ref "main";
  path "packages/deep/lib/alib.pkg.kio";
}
EOF
cat > "$scratch/consumer/blib.dep.kio" <<EOF
dependency blib;

source {
  git "$blib_bare";
  ref "main";
  path "packages/deep/lib/blib.pkg.kio";
}
EOF

cd "$scratch/consumer" || fail "cannot enter consumer"
"$KIO_BIN" dep fetch >/dev/null 2>fetch.err || {
  printf 'initial dep fetch failed:\n' >&2
  cat fetch.err >&2
  exit 1
}

alib_lock="alib.lock.kio"
blib_lock="blib.lock.kio"
grep -q "commit \"$sha_a_alib\";" "$alib_lock" || fail "alib not pinned to A; lockfile:
$(cat "$alib_lock")"
grep -q "commit \"$sha_a_blib\";" "$blib_lock" || fail "blib not pinned to A; lockfile:
$(cat "$blib_lock")"

# --- Advance both repos to commit B. ---
cp "$scratch/library/alib-commitB/util.kio" "$alib_src/util.kio" || fail "apply alib B"
(
  cd "$alib_src" || exit 1
  git add -A || exit 1
  GIT_AUTHOR_DATE='2020-01-02T00:00:00 +0000' \
  GIT_COMMITTER_DATE='2020-01-02T00:00:00 +0000' \
    git commit --quiet -m 'commit B' || exit 1
  git push --quiet "$alib_bare" main || exit 1
) || fail "cannot advance alib to B"
sha_b_alib=$(git --git-dir="$alib_bare" rev-parse HEAD) || fail "read alib B"
[ "$sha_a_alib" != "$sha_b_alib" ] || fail "alib A and B share a SHA"

cp "$scratch/library/blib-commitB/greet.kio" "$blib_src/greet.kio" || fail "apply blib B greet"
cp "$scratch/library/blib-commitB/blib.sig.kio" "$blib_src/blib.sig.kio" || fail "apply blib B sig"
(
  cd "$blib_src" || exit 1
  git add -A || exit 1
  GIT_AUTHOR_DATE='2020-01-02T00:00:00 +0000' \
  GIT_COMMITTER_DATE='2020-01-02T00:00:00 +0000' \
    git commit --quiet -m 'commit B' || exit 1
  git push --quiet "$blib_bare" main || exit 1
) || fail "cannot advance blib to B"
sha_b_blib=$(git --git-dir="$blib_bare" rev-parse HEAD) || fail "read blib B"
[ "$sha_a_blib" != "$sha_b_blib" ] || fail "blib A and B share a SHA"

# --- Subject: a plain `kio dep update` blocks on blib (exit 30) and, being
# atomic, advances NEITHER lock — alib (processed first, compatible) stays
# pinned to A. ---
"$KIO_BIN" dep update >block.out 2>block.err
rc=$?
[ "$rc" -eq 30 ] \
  || fail "the blocked multi-dep update should error with exit 30, got $rc; stderr:
$(cat block.err)"

grep -q "commit \"$sha_a_alib\";" "$alib_lock" \
  || fail "ATOMICITY VIOLATED: alib's lock advanced past A even though blib's gate blocked. \
A blocked update must leave every lock un-advanced. alib lockfile:
$(cat "$alib_lock")"
grep -q "commit \"$sha_a_blib\";" "$blib_lock" \
  || fail "the blocked update must leave blib pinned to A; blib lockfile:
$(cat "$blib_lock")"

# --- Heal: `--allow-breaking` adopts the break and advances BOTH locks. ---
"$KIO_BIN" dep update --allow-breaking >allow.out 2>allow.err
rc=$?
[ "$rc" -eq 0 ] \
  || fail "dep update --allow-breaking should succeed (exit $rc); stderr:
$(cat allow.err)"
grep -q "commit \"$sha_b_alib\";" "$alib_lock" \
  || fail "--allow-breaking did not advance alib to B; alib lockfile:
$(cat "$alib_lock")"
grep -q "commit \"$sha_b_blib\";" "$blib_lock" \
  || fail "--allow-breaking did not advance blib to B; blib lockfile:
$(cat "$blib_lock")"

# --- Build + run against the re-pinned dependencies. ---
"$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>build.err || {
  printf 'build failed after healed re-pin:\n' >&2
  cat build.err >&2
  exit 1
}
exec "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET"
