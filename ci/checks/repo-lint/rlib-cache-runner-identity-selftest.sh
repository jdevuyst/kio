#!/bin/sh

# The cache warm-hit check dispatches by runner identity, never by target ID.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
CHECK=${CHECK:-"$REPO_ROOT/ci/checks/per-case/rlib-cache-second-run-hits.sh"}

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

mkdir -p "$scratch/bin" "$scratch/case/workdir" "$scratch/tmp"
touch "$scratch/case/CACHE_HIT_ON_RERUN"
: >"$scratch/case/run.args"
: >"$scratch/prepared.args"

cat >"$scratch/bin/kio" <<'EOF'
#!/bin/sh
case "${1:-}" in
  build) exit 0 ;;
esac
exit 97
EOF

cat >"$scratch/bin/kio-test-runner-rust" <<'EOF'
#!/bin/sh
mkdir -p "$KIO_TEST_RUNNER_BUILD_CACHE_DIR/rlib/fixture"
: >"$KIO_TEST_RUNNER_BUILD_CACHE_DIR/rlib/fixture/bin"
: >"$KIO_TEST_RUNNER_CALLED"
EOF
chmod +x "$scratch/bin/kio" "$scratch/bin/kio-test-runner-rust"

if ! (
  cd "$scratch/case"
  TMPDIR="$scratch/tmp" \
    KIO_TARGET=custom-target \
    KIO_BIN="$scratch/bin/kio" \
    KIO_RUNNER="$scratch/bin/kio-test-runner-rust" \
    KIO_TEST_RUNNER_CACHE_KIND=rlib \
    KIO_TEST_RUN_ARGS_FILE="$scratch/prepared.args" \
    KIO_TEST_RUNNER_CALLED="$scratch/runner-called" \
    sh "$CHECK"
); then
  printf 'rlib-cache-runner-identity-selftest: custom target did not use the Rust runner cache adapter\n' >&2
  exit 1
fi

if [ ! -f "$scratch/runner-called" ]; then
  printf 'rlib-cache-runner-identity-selftest: cache check skipped a cache-backed runner with a custom target ID\n' >&2
  exit 1
fi

printf 'rlib-cache-runner-identity-selftest: ok\n'
