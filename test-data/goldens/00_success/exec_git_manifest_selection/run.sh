#!/bin/sh
# Two nested packages share a Git checkout and internal names but remain
# independent aliases. A shallow decoy must not affect explicit selection.
# Runtime output proves fetch pinning, update, and same-commit selector
# change/removal/addition. A missing selected manifest aborts all staged locks.
# Custom scripting supplies the hermetic file:// repository and moving ref.
# Direct Prime execution is excluded because ordinary calls infer type arguments.
set -u

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}
scratch=$(mktemp -d) || fail "cannot make fixture directory"
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/consumer" || fail "copy consumer"
cp -R library "$scratch/repository" || fail "copy repository"
KIO_CACHE_HOME="$scratch/cache"
export KIO_CACHE_HOME
GIT_AUTHOR_NAME='Kio Test'
GIT_AUTHOR_EMAIL='test@kio.invalid'
GIT_COMMITTER_NAME='Kio Test'
GIT_COMMITTER_EMAIL='test@kio.invalid'
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
repository="$scratch/repository"

commit_fixture() {
  (
    cd "$repository" || exit 1
    git add -A || exit 1
    GIT_AUTHOR_DATE="$1" GIT_COMMITTER_DATE="$1" git commit --quiet -m "$2"
  ) || fail "commit fixture"
}
git -C "$repository" init --quiet -b main || fail "initialize repository"
commit_fixture '2020-01-01T00:00:00 +0000' initial

write_dependency() {
  {
    printf 'dependency %s;\n\nsource {\n' "$1"
    printf '  git "file://%s";\n  ref "main"' "$repository"
    if [ -n "$2" ]; then
      printf ';\n  path "%s"' "$2"
    fi
    printf '\n}\n'
  } > "$scratch/consumer/$1.dep.kio"
}
left_path='packages/deep/left/lib.pkg.kio'
right_path='packages/deep/right/lib.pkg.kio'
write_dependency alib "$left_path"
write_dependency blib "$right_path"
cd "$scratch/consumer" || fail "enter consumer"

kio_ok() {
  "$KIO_BIN" "$@" >command.out 2>command.err || {
    cat command.err >&2
    fail "command failed: $*"
  }
}
run_pair() {
  kio_ok build "$KIO_TARGET"
  "$KIO_RUNNER" --protocol testapi-print "out/$KIO_TARGET" || fail "run consumer"
}
expect_stale() {
  "$KIO_BIN" dep fetch alib >command.out 2>command.err
  status=$?
  [ "$status" -eq 30 ] || fail "stale selector accepted: $status"
  grep -q 'manifest path' command.err || fail "missing selector diagnostic"
  grep -q 'dep update' command.err || fail "missing recovery instruction"
  kio_ok dep update alib
  grep -q 'first lock' command.out || fail "changed selector reported as unchanged"
}
kio_ok dep fetch
run_pair

# Swap the packages' behaviors at the next commit.
cp "$repository/packages/deep/left/greet.kio" "$scratch/left-before.kio" || fail "save left"
cp "$repository/packages/deep/right/greet.kio" "$repository/packages/deep/left/greet.kio" || fail "change left"
cp "$scratch/left-before.kio" "$repository/packages/deep/right/greet.kio" || fail "change right"
commit_fixture '2020-01-02T00:00:00 +0000' swapped
kio_ok dep fetch
run_pair
kio_ok dep update
run_pair

# A changed selector is a new source even at the same URL/ref/commit.
write_dependency alib "$right_path"
expect_stale
run_pair
write_dependency alib ''
expect_stale
run_pair
write_dependency alib "$left_path"
expect_stale
run_pair

# The first alias can stage a compatible move; the second loses its manifest.
cp alib.lock.kio "$scratch/alib-before.lock.kio" || fail "save first pin"
cp blib.lock.kio "$scratch/blib-before.lock.kio" || fail "save second pin"
mv "$repository/packages/deep/right/lib.pkg.kio" "$scratch/removed.pkg.kio" || fail "remove selected manifest"
commit_fixture '2020-01-03T00:00:00 +0000' missing
"$KIO_BIN" dep update >command.out 2>command.err
status=$?
[ "$status" -eq 30 ] || fail "missing selected manifest accepted: $status"
grep -q "$right_path" command.err || fail "missing selector context"
cmp -s alib.lock.kio "$scratch/alib-before.lock.kio" || fail "first pin advanced on failure"
cmp -s blib.lock.kio "$scratch/blib-before.lock.kio" || fail "second pin advanced on failure"
run_pair
