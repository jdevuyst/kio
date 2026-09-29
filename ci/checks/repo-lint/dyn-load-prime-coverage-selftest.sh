#!/bin/sh
#
# Verify the coverage lint's stable manifest read and execution bookkeeping.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
COVERAGE="$REPO_ROOT/ci/checks/repo-lint/dyn-load-prime-coverage.sh"

tmp_base=${KIO_TMP_DIR:-${TMPDIR:-/tmp}}
mkdir -p "$tmp_base"
scratch=$(mktemp -d "$tmp_base/dyn-load-prime-coverage-selftest.XXXXXX") || {
  printf 'dyn-load-prime-coverage-selftest: cannot make scratch dir\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

fixture="$scratch/repo"
fixture_case="$fixture/test-data/goldens/00_success/exec_church_numerals"
fixture_lint="$fixture/ci/checks/repo-lint/dyn-load-prime-coverage.sh"
manifest="$fixture_case/workdir/exec_church_numerals.pkg.kio"
mkdir -p "$(dirname "$fixture_lint")" \
  "$fixture/ci/infra/kio-test-runner-rs/dyn-load-prime-driver" \
  "$fixture/ci/infra/kio-test-runner-rs/src/shared" \
  "$(dirname "$fixture_case")"
cp "$COVERAGE" "$fixture_lint"
cp "$REPO_ROOT/ci/infra/kio-test-runner-rs/dyn-load-prime-driver/driver.kio" \
  "$fixture/ci/infra/kio-test-runner-rs/dyn-load-prime-driver/driver.kio"
cp "$REPO_ROOT/ci/infra/kio-test-runner-rs/src/shared/protocol.rs" \
  "$fixture/ci/infra/kio-test-runner-rs/src/shared/protocol.rs"
cp -R "$REPO_ROOT/test-data/goldens/00_success/exec_church_numerals" \
  "$fixture_case"

exact_fixture="$scratch/exact-repo"
cp -R "$fixture" "$exact_fixture"

real_cat=$(command -v cat)
real_grep=$(command -v grep)
fake_bin="$scratch/bin"
mkdir -p "$fake_bin"
cat >"$fake_bin/cat" <<'EOF'
#!/bin/sh
if [ "$#" -eq 1 ] && [ "$1" = "${DYN_COVERAGE_RACE_MANIFEST:-}" ]; then
  "$DYN_COVERAGE_REAL_CAT" "$1"
  rc=$?
  [ "$rc" -ne 0 ] || rm -f "$1"
  exit "$rc"
fi
exec "$DYN_COVERAGE_REAL_CAT" "$@"
EOF
cat >"$fake_bin/grep" <<'EOF'
#!/bin/sh
last=
for arg do last=$arg; done
if [ "$last" = "${DYN_COVERAGE_RACE_MANIFEST:-}" ]; then
  "$DYN_COVERAGE_REAL_GREP" "$@"
  rc=$?
  [ "$rc" -ne 0 ] || rm -f "$last"
  exit "$rc"
fi
exec "$DYN_COVERAGE_REAL_GREP" "$@"
EOF
chmod +x "$fake_bin/cat" "$fake_bin/grep"

log="$scratch/coverage.log"
rc=0
PATH="$fake_bin:$PATH" \
  DYN_COVERAGE_REAL_CAT="$real_cat" \
  DYN_COVERAGE_REAL_GREP="$real_grep" \
  DYN_COVERAGE_RACE_MANIFEST="$manifest" \
  sh "$fixture_lint" >"$log" 2>&1 || rc=$?

if [ "$rc" -ne 0 ]; then
  printf 'dyn-load-prime-coverage-selftest: FAIL — lint rejected a manifest removed after its stable read (exit %s)\n' "$rc" >&2
  cat "$log" >&2
  exit 1
fi
if [ -e "$manifest" ]; then
  printf 'dyn-load-prime-coverage-selftest: FAIL — fixture did not remove the manifest\n' >&2
  exit 1
fi

exact_case="$exact_fixture/test-data/goldens/00_success/exec_church_numerals"
exact_lint="$exact_fixture/ci/checks/repo-lint/dyn-load-prime-coverage.sh"
exact_main="$exact_case/workdir/testapi/main.kio"
exact_args="$exact_case/run.args"
original_main="$scratch/main.kio"
original_args="$scratch/run.args"
cp "$exact_main" "$original_main"
cp "$exact_args" "$original_args"

expect_exact_rejection() {
  label=$1
  rejection_log="$scratch/$label.log"
  rejection_rc=0
  sh "$exact_lint" >"$rejection_log" 2>&1 || rejection_rc=$?
  if [ "$rejection_rc" -eq 0 ]; then
    printf 'dyn-load-prime-coverage-selftest: FAIL — accepted %s drift\n' "$label" >&2
    cat "$rejection_log" >&2
    exit 1
  fi
  cp "$original_main" "$exact_main"
  cp "$original_args" "$exact_args"
}

expect_exact_acceptance() {
  label=$1
  acceptance_log="$scratch/$label.log"
  if ! sh "$exact_lint" >"$acceptance_log" 2>&1; then
    printf 'dyn-load-prime-coverage-selftest: FAIL — rejected %s\n' "$label" >&2
    cat "$acceptance_log" >&2
    exit 1
  fi
}

printf '%s\n' '--protocol not-a-runner-protocol' >"$exact_args"
expect_exact_rejection unknown-protocol

printf '%s\n' '--protocol testapi-array' >"$exact_args"
expect_exact_rejection protocol-without-complete-dyn-adapter

sed 's/^pub fn main/fn entry/' "$exact_main" >"$exact_main.new"
mv "$exact_main.new" "$exact_main"
printf '%s\n' '--protocol compile-only' >"$exact_args"
expect_exact_acceptance compile-only-without-main

printf '%s\n' '--protocol construct-only' >"$exact_args"
expect_exact_acceptance construct-only-without-main

: >"$exact_args"
expect_exact_acceptance default-protocol-is-runtime-validated

printf '%s\n' '--protocol empty-main' >"$exact_args"
expect_exact_acceptance explicit-main-protocol-is-runtime-validated

mkdir -p "$exact_fixture/test-data/goldens/01_error/stray"
: >"$exact_fixture/test-data/goldens/01_error/stray/DYN_LOAD_PRIME"
expect_exact_rejection marker-outside-success
if ! grep -Fq 'outside 00_success' "$scratch/marker-outside-success.log"; then
  cat "$scratch/marker-outside-success.log" >&2
  printf 'dyn-load-prime-coverage-selftest: FAIL — wrong stray-marker diagnostic\n' >&2
  exit 1
fi

printf 'dyn-load-prime-coverage-selftest: ok (manifest snapshot; protocol registry; support classification; success-only cohort)\n'
