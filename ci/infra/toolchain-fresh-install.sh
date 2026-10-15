#!/bin/sh
# Release preparation: no download/install cache, only tiny native witnesses.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
receipt=${1:?provide an absolute receipt directory}
: "${KIO_TOOLCHAINS_ROOT:?provide an empty absolute toolchain store}"
case "$receipt" in /*|[A-Za-z]:/*) ;; *) printf 'error: receipt directory must be absolute\n' >&2; exit 2 ;; esac
[ -d "$KIO_TOOLCHAINS_ROOT" ] || mkdir -p "$KIO_TOOLCHAINS_ROOT"
[ -z "$(ls -A "$KIO_TOOLCHAINS_ROOT")" ] || { printf 'error: fresh installer requires an empty store\n' >&2; exit 2; }
mkdir -p "$receipt"
cd "$repo_root"
started=$(date +%s)
finish() {
  status=$?
  {
    printf 'exit: %s\nelapsed-seconds: %s\n' "$status" "$(($(date +%s) - started))"
    du -sk "$KIO_TOOLCHAINS_ROOT"
    df -k "$KIO_TOOLCHAINS_ROOT"
  } >>"$receipt/receipt.txt"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
{
  printf 'source: %s\nplatform: %s %s\n' "$(git rev-parse HEAD)" "$(uname -s)" "$(uname -m)"
  printf 'cache-policy: empty owned manager, download, and installation state\n'
  df -k "$KIO_TOOLCHAINS_ROOT"
} >"$receipt/receipt.txt"
for input in mise*.toml mise*.lock tools/toolchains/* ci/impl-toolchain.sh ci/infra/toolchain-install-smoke.sh ci/infra/toolchain-fresh-install.sh; do
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$input"; else shasum -a 256 "$input"; fi
done >"$receipt/inputs.sha256"
sh ci/impl-toolchain.sh toolchain-bootstrap
languages=$(sh ci/impl-toolchain.sh supported-languages | tr '\n' ',')
sh ci/impl-toolchain.sh toolchain-plan "$languages" >"$receipt/plan.txt"
sh ci/infra/toolchain-install-smoke.sh "$languages"
