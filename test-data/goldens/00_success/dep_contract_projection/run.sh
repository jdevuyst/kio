#!/bin/sh
# The git-dependency commands compute an unsealed dependency's contract from
# signatures alone. This case pins the real CLI path: body-only imports of
# surface declarations do not enter the digest, alpha-renaming universal and
# existential binders keeps it stable, and invalid signature declarations
# reject a later re-pin before the lock moves.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

command -v git >/dev/null 2>&1 || fail "git is required for this test"

scratch=$(mktemp -d) || fail "cannot make scratch directory"
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

KIO_CACHE_HOME="$scratch/kio-cache"
export KIO_CACHE_HOME
unset XDG_CACHE_HOME

cp -R workdir "$scratch/consumer" || fail "cannot copy consumer"
cp -R library "$scratch/library" || fail "cannot copy dependency fixtures"

GIT_AUTHOR_NAME='Kio Test'
GIT_AUTHOR_EMAIL='test@kio.invalid'
GIT_COMMITTER_NAME='Kio Test'
GIT_COMMITTER_EMAIL='test@kio.invalid'
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL

src="$scratch/library/contract"
(
  cd "$src" || exit 1
  git init --quiet -b main . || exit 1
  git add -A || exit 1
  GIT_AUTHOR_DATE='2020-01-01T00:00:00 +0000' \
  GIT_COMMITTER_DATE='2020-01-01T00:00:00 +0000' \
    git commit --quiet -m 'baseline contract' || exit 1
) || fail "cannot assemble baseline dependency"

bare="$scratch/contract.git"
git clone --quiet --bare "$src" "$bare" || fail "cannot create dependency repository"

cat > "$scratch/consumer/contract.dep.kio" <<EOF
dependency contract;

source {
  git "$bare";
  ref "main";
}
EOF

cd "$scratch/consumer" || fail "cannot enter consumer"
"$KIO_BIN" dep fetch >fetch.out 2>fetch.err || fail "initial dep fetch failed: $(cat fetch.err)"
lock=contract.lock.kio
sig_a=$(grep 'sig ' "$lock") || fail "baseline lock recorded no contract digest"

publish() {
  fixture=$1
  date=$2
  message=$3
  cp "$scratch/library/$fixture/api.kio" "$src/api.kio" || return 1
  (
    cd "$src" || exit 1
    git add api.kio || exit 1
    GIT_AUTHOR_DATE="$date" GIT_COMMITTER_DATE="$date" \
      git commit --quiet -m "$message" || exit 1
    git push --quiet "$bare" main || exit 1
  )
}

publish commit_b '2020-01-02T00:00:00 +0000' 'rename contract binders' \
  || fail "cannot publish alpha-renamed contract"
sha_b=$(git --git-dir="$bare" rev-parse HEAD) || fail "cannot read renamed commit"
"$KIO_BIN" dep update >rename.out 2>rename.err \
  || fail "alpha-equivalent dep update failed: $(cat rename.err)"
grep -Fq 'breaking' rename.err \
  && fail "alpha-equivalent binder rename was reported as breaking: $(cat rename.err)"
sig_b=$(grep 'sig ' "$lock") || fail "renamed lock recorded no contract digest"
[ "$sig_a" = "$sig_b" ] \
  || fail "universal/existential binder rename changed the contract digest"
grep -Fq "commit \"$sha_b\";" "$lock" || fail "lock did not advance to renamed contract"

expect_invalid() {
  fixture=$1
  date=$2
  message=$3
  expected=$4
  publish "$fixture" "$date" "$message" || fail "cannot publish $fixture contract"
  "$KIO_BIN" dep update >"$fixture.out" 2>"$fixture.err"
  rc=$?
  [ "$rc" -eq 30 ] \
    || fail "$fixture contract should fail with dependency exit 30, got $rc: $(cat "$fixture.err")"
  grep -Fq "$expected" "$fixture.err" \
    || fail "$fixture contract did not report '$expected': $(cat "$fixture.err")"
  grep -Fq "commit \"$sha_b\";" "$lock" \
    || fail "$fixture failure moved the lock away from the last valid contract"
}

# shellcheck disable=SC2016 # backticks are literal diagnostic text
expect_invalid invalid_modifier '2020-01-03T00:00:00 +0000' \
  'invalid pure modifier' '`pure` is valid only on ordinary `fn` declarations'
# shellcheck disable=SC2016 # backticks are literal diagnostic text
expect_invalid invalid_arity '2020-01-04T00:00:00 +0000' \
  'invalid signature arity' 'type alias `Box` requires 1 type argument, but 2 were supplied'
expect_invalid invalid_rec '2020-01-05T00:00:00 +0000' \
  'invalid recursive header' 'mutual-recursion members must declare the same type parameters'

printf 'dependency contract projection ok\n'
