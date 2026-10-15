#!/bin/sh
# Bounded current-generation and unchanged-host witnesses; no installation.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
usage() {
  printf 'Usage: sh ci/infra/check-baseline-compatibility.sh LANGUAGE BASELINE [--endpoint minimum|current-tested|both] [--plan]\n'
}
[ "$#" -ge 2 ] || { usage >&2; exit 2; }
language=$1 baseline=$2
shift 2
endpoint=both
plan=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --endpoint) shift; [ "$#" -gt 0 ] || { usage >&2; exit 2; }; endpoint=$1 ;;
    --endpoint=*) endpoint=${1#*=} ;;
    --plan) plan=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
  shift
done
environments=$(sh "$script_dir/baseline-environments.sh" resolve "$language" "$baseline" "$endpoint") || exit 2
witness=$(awk -v backend="$language" -v baseline="$baseline" '$1 == backend && $2 == baseline { print $3 }' "$script_dir/baseline-witnesses.tsv")
driver_requirement=$(awk -v backend="$language" -v baseline="$baseline" '$1 == backend && $2 == baseline { print $4 }' "$script_dir/baseline-witnesses.tsv")
if [ -z "$witness" ] || [ ! -f "$repo_root/test-data/emissions/$language/$witness/HOST_INTERFACE" ]; then
  printf 'error: no bounded host-interface witness for %s\n' "$language" >&2; exit 2
fi
for environment in $environments; do
  printf 'baseline-compatibility: backend=%s baseline=%s environment=%s witness=%s/%s driver=%s\n' \
    "$language" "$baseline" "$environment" "$language" "$witness" "$driver_requirement"
  sh "$repo_root/ci/impl-toolchain.sh" toolchain-plan "$language" --environment "$environment"
  [ "$plan" -eq 0 ] || continue
  # Historical Node executes only this trusted, offline host fixture. Other
  # native platforms need their own explicit availability/acceptance evidence.
  if [ "$environment" = node14 ]; then
    case "$(uname -s):$(uname -m)" in
      Linux:x86_64) ;;
      *) printf 'error: the retained Node minimum witness is accepted only on Linux x64; native Apple Silicon has no official Node14 binary\n' >&2; exit 2 ;;
    esac
  fi
  # The orchestrator prepares repository binaries before the harness selects
  # subject tools. Endpoint selection never changes its Cargo build toolchain.
  sh "$repo_root/ci/checks/orchestrators/emissions-tests.sh" --impls="kio@$language" \
    --all-cases -- "--toolchain-environment=$environment" "^$language/$witness$"
done
printf 'baseline-compatibility: current generated output and fixed host source; retained prior output is a separate witness\n'
