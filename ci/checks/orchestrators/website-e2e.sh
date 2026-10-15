#!/bin/sh
#
# Build and smoke-test the Kio website exactly as the Pages workflow does.
#
# POSIX sh only.

set -eu

if [ $# -gt 0 ]; then
  case "$1" in
    -h|--help)
      cat <<'EOF'
Usage: sh ci/checks/orchestrators/website-e2e.sh

Install website dependencies, build the wasm inspector bundle, build the
VitePress site, smoke-check the generated artifacts, and exercise the
real page/worker/WASM REPL in mise-managed Firefox.
EOF
      exit 0
      ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
WEBSITE_DIR="$REPO_ROOT/website"

for tool in node npm wasm-pack cargo mise; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'website-e2e: %s not found on PATH.\n' "$tool" >&2
    printf '%s\n' '  run sh ci/impl-toolchain.sh install-core, then sh ci/impl-toolchain.sh toolchain-exec js,rust --core -- sh ci/checks/orchestrators/website-e2e.sh from the repo root.' >&2
    exit 2
  fi
done

if ! (
  cd "$REPO_ROOT"
  if [ -n "${KIO_GENERAL_MISE_DATA_DIR:-}" ]; then
    export MISE_DATA_DIR="$KIO_GENERAL_MISE_DATA_DIR"
  fi
  mise where http:kio-firefox
) >/dev/null 2>&1; then
  printf '%s\n' 'website-e2e: mise-managed Firefox is missing; run mise install --locked http:kio-firefox.' >&2
  exit 2
fi

cd "$WEBSITE_DIR"
npm ci --no-fund --no-audit
npm run check:examples
npm run build
npm run audit
npm run smoke
npm run browser:smoke
npm run browser:history
