#!/bin/sh
# Human-facing selection; ci/impl-toolchain.sh owns provisioning.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
usage() {
  printf '%s\n' \
    'Usage: sh ci/install-target-language-toolchains.sh <language>... [--environment ID] [--plan]' \
    '       sh ci/install-target-language-toolchains.sh --supported-set|--portability-set [--plan]' \
    '       sh ci/install-target-language-toolchains.sh --list' \
    '' \
    'Languages: js ts python rust go java swift haskell' \
    'No arguments display these choices. Ordinary setup does not install every language.' \
    'Use ci/impl-toolchain.sh toolchain-exec <languages> [--environment ID] -- command to execute.'
}
[ $# -gt 0 ] || { usage; exit 0; }
operation=toolchain-install
languages=
environment=development
selected_set=
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --list)
      printf 'Supported on this OS:\n'; sh "$script_dir/impl-toolchain.sh" supported-languages
      printf 'Portability set:\n'; sh "$script_dir/impl-toolchain.sh" portability-languages
      printf 'Exact environments:\n'; sh "$script_dir/impl-toolchain.sh" toolchain-environments
      exit 0 ;;
    --plan) operation=toolchain-plan ;;
    --supported-set|--portability-set)
      [ -z "$selected_set$languages" ] || { printf 'error: choose one set or explicit languages\n' >&2; exit 2; }
      selected_set=$1 ;;
    --environment) shift; [ $# -gt 0 ] || { printf 'error: --environment requires an ID\n' >&2; exit 2; }; environment=$1 ;;
    --environment=*) environment=${1#*=} ;;
    --*) printf 'error: unknown installer argument: %s\n' "$1" >&2; exit 2 ;;
    *)
      [ -z "$selected_set" ] || { printf 'error: choose one set or explicit languages\n' >&2; exit 2; }
      languages="${languages:+$languages,}$1" ;;
  esac
  shift
done
case "$selected_set" in
  --supported-set) languages=$(sh "$script_dir/impl-toolchain.sh" supported-languages | tr '\n' ',') ;;
  --portability-set) languages=$(sh "$script_dir/impl-toolchain.sh" portability-languages | tr '\n' ',') ;;
esac
[ -n "$languages" ] || { usage >&2; exit 2; }
exec sh "$script_dir/impl-toolchain.sh" "$operation" "$languages" --environment "$environment"
