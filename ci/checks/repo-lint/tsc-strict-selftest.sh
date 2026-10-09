#!/bin/sh
# Exercise the real per-case TS declaration gate without compiling a package.
# POSIX sh only.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
if command -v mise >/dev/null 2>&1 && SELFTEST_NODE=$(cd "$REPO_ROOT" && mise which node 2>/dev/null) && [ -n "$SELFTEST_NODE" ]; then
  :
else
  SELFTEST_NODE=$(command -v node)
fi
export SELFTEST_NODE
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
mkdir -p "$scratch/bin" "$scratch/case/workdir" "$scratch/no-workdir"

cat >"$scratch/bin/kio" <<'MOCK'
#!/bin/sh
set -eu
[ "$*" != 'debug build-output-plan ts' ] || {
  printf '[{"package_root":"%s","target":"ts","backend":"ts","output_dir":"%s/out/ts","namespace":"pkg"}]\n' "$PWD" "$PWD"
  exit 0
}
[ "$*" = 'build ts' ] || exit 98
printf 'build\n' >>"$TSC_SELFTEST_EVENTS"
[ "$TSC_SELFTEST_MODE" != build-failure ] || exit 14
if [ "$TSC_SELFTEST_MODE" != missing ]; then
  mkdir -p out/ts
  printf 'export const value = 1;\n' >out/ts/pkg.js
  printf 'export declare const value: number;\n' >out/ts/pkg.d.ts
fi
MOCK
cat >"$scratch/bin/tsc" <<'MOCK'
#!/bin/sh
set -eu
[ "$#" = 3 ] && [ "$1" = --strict ] && [ "$2" = --noEmit ] || exit 98
[ -f "$3" ] || exit 98
printf 'tsc\n' >>"$TSC_SELFTEST_EVENTS"
[ "$TSC_SELFTEST_MODE" != invalid ]
MOCK
cat >"$scratch/bin/mise" <<'MOCK'
#!/bin/sh
[ "$*" != 'which node' ] || { printf '%s\n' "$SELFTEST_NODE"; exit 0; }
[ "$*" = 'which tsc' ] || exit 98
printf '%s/tsc\n' "${0%/*}"
MOCK
chmod +x "$scratch/bin/kio" "$scratch/bin/tsc" "$scratch/bin/mise"
PATH="$scratch/bin:$PATH"
export PATH
KIO_BIN="$scratch/bin/kio"
TSC_SELFTEST_EVENTS="$scratch/events"
export KIO_BIN TSC_SELFTEST_EVENTS

check() {
  TSC_SELFTEST_MODE=$1
  KIO_TARGET=$2
  expected=$3
  expected_events=$4
  export TSC_SELFTEST_MODE KIO_TARGET
  : >"$TSC_SELFTEST_EVENTS"
  status=0
  (cd "$5" && sh "$REPO_ROOT/ci/checks/per-case/tsc-strict.sh") \
    >"$scratch/stdout" 2>"$scratch/stderr" || status=$?
  if [ "$status" != "$expected" ]; then
    printf 'FAIL %s/%s: expected exit %s, got %s\n' "$1" "$2" "$expected" "$status" >&2
    cat "$scratch/stderr" >&2
    exit 1
  fi
  actual_events=$(paste -sd, "$TSC_SELFTEST_EVENTS")
  [ "$actual_events" = "$expected_events" ] || {
    printf 'FAIL %s/%s: unexpected invocations %s\n' "$1" "$2" "$actual_events" >&2
    exit 1
  }
  if [ "$1" = missing ] && [ "$2" = ts ]; then
    grep -q 'no declaration sidecar' "$scratch/stderr"
  fi
}

check missing ts 1 build "$scratch/case"
check invalid ts 1 build,tsc "$scratch/case"
check valid ts 0 build,tsc "$scratch/case"
check build-failure ts 0 build "$scratch/case"
check missing js 0 '' "$scratch/case"
check valid ts 0 '' "$scratch/no-workdir"
printf 'tsc-strict selftest: 6 controls passed\n'
