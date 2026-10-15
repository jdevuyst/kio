#!/bin/sh
# Human-facing selection; ci/impl-toolchain.sh owns provisioning.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
usage() {
  printf '%s\n' \
    'Usage: sh ci/install-target-language-toolchains.sh <language>... [--environment ID|--for-tests] [--plan]' \
    '       sh ci/install-target-language-toolchains.sh --supported-set|--portability-set [--for-tests] [--plan]' \
    '       sh ci/install-target-language-toolchains.sh <language> --baseline BASELINE [--endpoint minimum|current-tested|both] [--plan]' \
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
environment_set=0
baseline=
baseline_set=0
endpoint=current-tested
endpoint_set=0
for_tests=0
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
    --for-tests) for_tests=1 ;;
    --supported-set|--portability-set)
      [ -z "$selected_set$languages" ] || { printf 'error: choose one set or explicit languages\n' >&2; exit 2; }
      selected_set=$1 ;;
    --environment) shift; [ $# -gt 0 ] || { printf 'error: --environment requires an ID\n' >&2; exit 2; }; environment=$1; environment_set=1 ;;
    --environment=*) environment=${1#*=}; environment_set=1 ;;
    --baseline) shift; [ $# -gt 0 ] || { printf 'error: --baseline requires a name\n' >&2; exit 2; }; baseline=$1; baseline_set=1 ;;
    --baseline=*) baseline=${1#*=}; baseline_set=1 ;;
    --endpoint) shift; [ $# -gt 0 ] || { printf 'error: --endpoint requires a name\n' >&2; exit 2; }; endpoint=$1; endpoint_set=1 ;;
    --endpoint=*) endpoint=${1#*=}; endpoint_set=1 ;;
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
if [ "$for_tests" -eq 1 ] && { [ "$baseline_set" -eq 1 ] || [ "$environment_set" -eq 1 ]; }; then
  printf 'error: --for-tests cannot be combined with --baseline or --environment\n' >&2; exit 2
fi
if [ "$baseline_set" -eq 1 ]; then
  [ "$environment_set" -eq 0 ] || { printf 'error: --baseline and --environment are mutually exclusive\n' >&2; exit 2; }
  case "$languages:$selected_set" in
    *','*:*|*:*?*) printf 'error: --baseline requires one explicit language\n' >&2; exit 2 ;;
  esac
elif [ "$endpoint_set" -eq 1 ]; then
  printf 'error: --endpoint requires --baseline\n' >&2; exit 2
fi
if [ "$for_tests" -eq 1 ]; then
  requirements=$(sh "$script_dir/infra/baseline-environments.sh" requirements "${languages%,}" sample) || exit 2
  while IFS="$(printf '\t')" read -r language environment; do
    sh "$script_dir/impl-toolchain.sh" "$operation" "$language" --environment "$environment" || exit
  done <<EOF
$requirements
EOF
  exit 0
fi
if [ "$baseline_set" -eq 1 ]; then
  environments=$(sh "$script_dir/infra/baseline-environments.sh" resolve "$languages" "$baseline" "$endpoint") || exit 2
  for environment in $environments; do
    sh "$script_dir/impl-toolchain.sh" "$operation" "$languages" --environment "$environment" || exit
  done
  exit 0
fi
exec sh "$script_dir/impl-toolchain.sh" "$operation" "$languages" --environment "$environment"
