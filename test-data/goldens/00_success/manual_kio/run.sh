#!/bin/sh
# The installed command exposes the complete tracked manual from an unrelated
# directory. Host tar checks complement the Rust archive-invariant tests;
# native OS extraction acceptance is recorded separately.
set -eu

fail() { printf '%s\n' "$1" >&2; exit 1; }

root=$(pwd)
while [ "$root" != / ] && [ ! -f "$root/KIO-FOR-AGENTS.md" ]; do
  root=$(dirname "$root")
done
[ -f "$root/KIO-FOR-AGENTS.md" ] || fail 'cannot locate manual source oracle'
mkdir -p out
workspace=$(mktemp -d out/manual.XXXXXX)
mkdir "$workspace/unrelated"
cd "$workspace/unrelated"

git -C "$root" ls-files -- docs specs KIO-FOR-AGENTS.md | LC_ALL=C sort >../inventory
"$KIO_BIN" manual ls >../root-list
printf 'KIO-FOR-AGENTS.md\ndocs/\nspecs/\n' >../expected-list
cmp ../expected-list ../root-list
"$KIO_BIN" --no-cache manual tree --no-cache >../tree
"$KIO_BIN" manual tree . >../root-tree
cmp ../tree ../root-tree
"$KIO_BIN" manual ls docs/ >../docs-list
"$KIO_BIN" manual ls docs >../docs-list-plain
cmp ../docs-list ../docs-list-plain

while IFS= read -r path; do
  "$KIO_BIN" manual cat "$path" >../file
  cmp "$root/$path" ../file || fail "cat bytes differ: $path"
done <../inventory

for format in tar.gz-concat tar tar.gz; do
  "$KIO_BIN" manual export --format="$format" >"../archive.$format"
  "$KIO_BIN" manual export --format "$format" >../repeat
  cmp "../archive.$format" ../repeat
  mkdir "../extract.$format"
  case "$format" in
    tar.gz-concat)
      tar --ignore-zeros -tzf "../archive.$format" >../export-list
      tar --ignore-zeros -xzf "../archive.$format" -C "../extract.$format"
      ;;
    tar)
      tar -tf "../archive.$format" >../export-list
      tar -xf "../archive.$format" -C "../extract.$format"
      ;;
    tar.gz)
      tar -tzf "../archive.$format" >../export-list
      tar -xzf "../archive.$format" -C "../extract.$format"
      ;;
  esac
  cmp ../inventory ../export-list || fail "export inventory differs: $format"
  while IFS= read -r path; do
    cmp "$root/$path" "../extract.$format/$path" || fail "export bytes differ: $format $path"
  done <../inventory
done
"$KIO_BIN" manual export >../default
cmp ../archive.tar.gz-concat ../default

reject() {
  set +e
  "$KIO_BIN" manual "$@" >../rejected.stdout 2>../rejected.stderr
  status=$?
  set -e
  [ "$status" = 2 ] || fail "expected usage exit2 for manual $*; got $status"
  [ ! -s ../rejected.stdout ] || fail "usage error wrote stdout: $*"
  [ -s ../rejected.stderr ] || fail "usage error omitted diagnostic: $*"
}
reject cat
reject cat docs
reject cat docs/README.md/
reject cat /docs/README.md
reject cat docs/../README.md
reject cat docs//README.md
reject cat 'docs\README.md'
reject cat docs/missing.md
reject ls KIO-FOR-AGENTS.md
reject tree docs extra
reject search
reject export --format=zip
reject export extra

# Completion offers virtual operations, not host filenames or export-only flags
# at other nodes. Exercise bash's actual function rather than only its text.
if command -v bash >/dev/null 2>&1; then
  "$KIO_BIN" completions bash >../completion.bash
  bash -s ../completion.bash <<'BASH'
set -eu
. "$1"
check() {
  COMP_CWORD=$(($# - 1))
  COMP_WORDS=("$@")
  COMPREPLY=()
  _kio
}
check kio manual ''
for operation in ls tree cat export; do
  [[ " ${COMPREPLY[*]} " == *" $operation "* ]] || exit 1
done
[[ " ${COMPREPLY[*]} " != *--format* ]]
check kio manual export --format ''
[[ " ${COMPREPLY[*]} " == *' tar.gz-concat '* ]]
[[ " ${COMPREPLY[*]} " == *' tar '* ]]
[[ " ${COMPREPLY[*]} " == *' tar.gz '* ]]
check kio manual cat ''
[[ " ${COMPREPLY[*]} " != *--format* ]]
[[ " ${COMPREPLY[*]} " != *' export '* ]]
BASH
fi
