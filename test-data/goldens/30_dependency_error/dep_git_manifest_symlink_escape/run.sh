#!/bin/sh
# A symlinked manifest outside the checkout is rejected.
# A custom script supplies a hermetic file:// repository. Selection fails
# before a first lock or materialized tree is written, including under --force.
# This witness requires real filesystem symlinks; copying a link's contents
# cannot count as containment evidence, including on Windows shell routes.
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
GIT_CONFIG_COUNT=1
GIT_CONFIG_KEY_0=core.symlinks
GIT_CONFIG_VALUE_0=true
export GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0
cp "$repository/lib.pkg.kio" "$scratch/outside.pkg.kio" || fail "copy outside manifest"
ln -s "$scratch/outside.pkg.kio" "$repository/escaped.pkg.kio" || fail "create escape link"
[ -L "$repository/escaped.pkg.kio" ] || fail "fixture requires native filesystem symlink support"
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
  printf '  path "escaped.pkg.kio"\n}\n'
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
  selected_link=$(find "$KIO_CACHE_HOME" -name escaped.pkg.kio -type l -print) || fail "inspect isolated checkout"
  if [ -z "$selected_link" ] || [ ! -L "$selected_link" ]; then
    fail "Git checkout must preserve the native escaping symlink"
  fi
done
cat command.err >&2
exit "$status"
