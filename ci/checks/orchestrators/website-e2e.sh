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
VitePress site, and smoke-check the generated artifacts.
EOF
      exit 0
      ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
WEBSITE_DIR="$REPO_ROOT/website"

for tool in node npm wasm-pack cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'website-e2e: %s not found on PATH.\n' "$tool" >&2
    printf '%s\n' '  run mise install --locked from the repo root, or use the devcontainer.' >&2
    exit 2
  fi
done

cd "$WEBSITE_DIR"
npm ci --no-fund --no-audit
npm run check:examples
npm run build
npm run audit
npm run smoke
