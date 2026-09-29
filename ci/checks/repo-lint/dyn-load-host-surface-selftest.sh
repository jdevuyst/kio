#!/bin/sh
#
# Verify cohort discovery and exact bridge-surface rejection.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
LINT="$REPO_ROOT/ci/checks/repo-lint/dyn-load-host-surface.sh"

tmp_base=${KIO_TMP_DIR:-${TMPDIR:-/tmp}}
mkdir -p "$tmp_base"
scratch=$(mktemp -d "$tmp_base/dyn-load-host-surface-selftest.XXXXXX") || {
  printf 'dyn-load-host-surface-selftest: cannot make scratch dir\n' >&2
  exit 2
}
cleanup_dyn_load_host_surface_selftest() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_dyn_load_host_surface_selftest EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fixture="$scratch/repo"
fixture_lint="$fixture/ci/checks/repo-lint/dyn-load-host-surface.sh"
cases_root="$fixture/test-data/goldens/00_success"
selected="$cases_root/exec_dyn_load_selected"
equals="$cases_root/exec_dyn_load_equals"
bad_source="$cases_root/exec_dyn_load_bad_source"
bad_protocol="$cases_root/exec_dyn_load_bad_protocol"
unrelated="$cases_root/other_case"
mkdir -p "$(dirname "$fixture_lint")"
for case_dir in "$selected" "$equals" "$bad_source" "$bad_protocol" "$unrelated"; do
  mkdir -p "$case_dir/workdir"
done
cp "$LINT" "$fixture_lint"

cat >"$selected/run.sh" <<'EOF'
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
"$KIO_RUNNER" --package-name selected \
  --protocol testapi-dyn-load out
EOF
cat >"$selected/workdir/selected.pkg.kio" <<'EOF'
package selected;
bridge {
  testapi;
  testapi/arith;
  testapi/fmt;
  testapi/io;
  testapi/iter;
  testapi/main;
  testapi/scalar;
  testapi/text;
}
EOF
cp "$selected/workdir/selected.pkg.kio" "$equals/workdir/equals.pkg.kio"
cp "$selected/workdir/selected.pkg.kio" "$bad_source/workdir/bad_source.pkg.kio"
cp "$selected/workdir/selected.pkg.kio" "$bad_protocol/workdir/bad_protocol.pkg.kio"

cat >"$equals/run.sh" <<'EOF'
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
"$KIO_RUNNER" --protocol=testapi-dyn-load out
EOF
cp "$selected/run.sh" "$bad_source/run.sh"
cp "$selected/run.sh" "$bad_protocol/run.sh"

# A broad bridge outside the structural cohort is not this lint's concern.
cat >"$unrelated/run.sh" <<'EOF'
"$KIO_RUNNER" --protocol testapi-print
EOF
cat >"$unrelated/workdir/unrelated.pkg.kio" <<'EOF'
package unrelated;
bridge {
  testapi;
  loader;
}
EOF

expect_acceptance() {
  label=$1
  log="$scratch/$label.log"
  if ! sh "$fixture_lint" >"$log" 2>&1; then
    printf 'dyn-load-host-surface-selftest: FAIL — rejected %s\n' "$label" >&2
    cat "$log" >&2
    exit 1
  fi
  if ! grep -Fq 'OK (4 scripted hosts)' "$log"; then
    printf 'dyn-load-host-surface-selftest: FAIL — wrong cohort for %s\n' "$label" >&2
    cat "$log" >&2
    exit 1
  fi
}

expect_rejection() {
  label=$1
  diagnostic=$2
  log="$scratch/$label.log"
  rc=0
  sh "$fixture_lint" >"$log" 2>&1 || rc=$?
  if [ "$rc" -eq 0 ]; then
    printf 'dyn-load-host-surface-selftest: FAIL — accepted %s\n' "$label" >&2
    cat "$log" >&2
    exit 1
  fi
  if ! grep -Fq "$diagnostic" "$log"; then
    printf 'dyn-load-host-surface-selftest: FAIL — wrong diagnostic for %s\n' "$label" >&2
    cat "$log" >&2
    exit 1
  fi
}

manifest="$selected/workdir/selected.pkg.kio"
original="$scratch/selected.pkg.kio"
cp "$manifest" "$original"

expect_acceptance exact-surface

sed '/testapi\/text;/a\
  loader;' "$original" >"$manifest"
expect_rejection extra-internal-module 'exec_dyn_load_selected: bridge surface must exactly match'

sed '/testapi\/text;/d' "$original" >"$manifest"
expect_rejection missing-host-module 'exec_dyn_load_selected: bridge surface must exactly match'

cp "$original" "$manifest"
expect_acceptance restored-surface

mkdir -p "$selected/workdir/nested"
cp "$original" "$selected/workdir/nested/nested.pkg.kio"
expect_acceptance nested-manifest-ignored

cp "$original" "$selected/workdir/extra.pkg.kio"
expect_rejection multiple-root-manifests 'exec_dyn_load_selected: expected exactly one root package manifest, found 2'
rm "$selected/workdir/extra.pkg.kio"

cat >"$bad_source/run.sh" <<'EOF'
PKG_SRC="$REPO_ROOT/test-data/poc/other/workdir"
"$KIO_RUNNER" --protocol testapi-dyn-load out
EOF
expect_rejection noncanonical-source 'exec_dyn_load_bad_source: PKG_SRC must name the canonical dyn_load_prime workdir'
cp "$selected/run.sh" "$bad_source/run.sh"

cat >"$bad_protocol/run.sh" <<'EOF'
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
# "$KIO_RUNNER" --protocol testapi-dyn-load out
"$KIO_RUNNER" --protocol testapi-print out
EOF
expect_rejection comment-only-protocol 'exec_dyn_load_bad_protocol: run.sh must invoke KIO_RUNNER with the testapi-dyn-load protocol'
cp "$selected/run.sh" "$bad_protocol/run.sh"

expect_acceptance restored-predicates

printf 'dyn-load-host-surface-selftest: ok (structural cohort; fail-closed predicates; exact root surface)\n'
