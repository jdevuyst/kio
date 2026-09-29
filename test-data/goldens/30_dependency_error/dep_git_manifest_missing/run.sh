#!/bin/sh
# A missing explicit manifest is rejected despite a valid shallow decoy.
# A custom script supplies a hermetic file:// repository. Selection fails
# before a first lock or materialized tree is written, including under --force.
set -u
fail() {
  printf '%s\n' "$*" >&2
  exit 1
}
scratch=$(mktemp -d) || fail "create fixture directory"
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/consumer" || fail "copy consumer"
cp -R library "$scratch/repository" || fail "copy repository"
repository="$scratch/repository"
KIO_CACHE_HOME="$scratch/cache"
export KIO_CACHE_HOME
git -C "$repository" init --quiet -b main || fail "initialize repository"
git -C "$repository" add -A || fail "stage fixture"
GIT_AUTHOR_NAME='Kio Test' GIT_AUTHOR_EMAIL='test@kio.invalid' \
GIT_COMMITTER_NAME='Kio Test' GIT_COMMITTER_EMAIL='test@kio.invalid' \
GIT_AUTHOR_DATE='2020-01-01T00:00:00 +0000' \
GIT_COMMITTER_DATE='2020-01-01T00:00:00 +0000' \
  git -C "$repository" commit --quiet -m fixture || fail "commit fixture"
{
  printf 'dependency lib;\n\nsource {\n'
  printf '  git "file://%s";\n  ref "main";\n' "$repository"
  printf '  path "packages/missing/lib.pkg.kio"\n}\n'
} > "$scratch/consumer/lib.dep.kio"
cd "$scratch/consumer" || fail "enter consumer"
for force in '' '--force'; do
  if [ -n "$force" ]; then
    "$KIO_BIN" dep fetch "$force" >command.out 2>command.err
  else
    "$KIO_BIN" dep fetch >command.out 2>command.err
  fi
  status=$?
  [ "$status" -eq 30 ] || { cat command.err >&2; fail "expected dependency error, got $status"; }
  [ ! -e lib.lock.kio ] || fail "invalid selector wrote a lock"
  [ ! -e lib ] || fail "invalid selector materialized modules"
done
cat command.err >&2
exit "$status"
