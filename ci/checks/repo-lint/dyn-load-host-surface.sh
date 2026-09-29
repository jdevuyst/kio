#!/bin/sh
#
# Keep scripted dyn_load_prime hosts on the fixed runner's exact public
# surface. The host package still imports, typechecks, and emits the complete
# loader implementation. Listing implementation modules in `bridge`, however,
# also generates public host facades for thousands of entries the runner never
# uses, multiplying native compiler time and memory without adding coverage.
#
# The structural cohort is every exec_dyn_load_* success golden with a custom
# run.sh. Each member must vendor the canonical dyn_load_prime package and
# invoke the fixed testapi-dyn-load runner protocol. Those properties are
# validated fail-closed rather than used to make a case disappear from the
# cohort.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
CASES_ROOT="$REPO_ROOT/test-data/goldens/00_success"

expected_bridge_entries='testapi
testapi/arith
testapi/fmt
testapi/io
testapi/iter
testapi/main
testapi/scalar
testapi/text'

status=0
checked=0

fail() {
  printf 'dyn-load-host-surface: %s\n' "$1" >&2
  status=1
}

for run_script in "$CASES_ROOT"/exec_dyn_load_*/run.sh; do
  [ -f "$run_script" ] || continue
  case_dir=${run_script%/run.sh}
  case_name=$(basename "$case_dir")

  if ! grep -Eq \
    '^[[:space:]]*PKG_SRC="[$]REPO_ROOT/test-data/poc/dyn_load_prime/workdir"[[:space:]]*$' \
    "$run_script"; then
    fail "$case_name: PKG_SRC must name the canonical dyn_load_prime workdir"
    continue
  fi

  logical_lines=$(
    awk '
      function emit() {
        if (logical != "") print logical
        logical = ""
      }
      {
        line = $0
        if (line ~ /^[[:space:]]*#/) next
        sub(/[[:space:]]+#.*/, "", line)
        if (logical == "") logical = line
        else logical = logical " " line
        if (logical ~ /\\[[:space:]]*$/) {
          sub(/\\[[:space:]]*$/, "", logical)
          next
        }
        emit()
      }
      END { emit() }
    ' "$run_script"
  )
  if ! printf '%s\n' "$logical_lines" \
    | grep -Eq -- '(^|[|;])[[:space:]]*"[$]KIO_RUNNER"[[:space:]]+.*--protocol([[:space:]]+|=)testapi-dyn-load([[:space:]]|$)'; then
    fail "$case_name: run.sh must invoke KIO_RUNNER with the testapi-dyn-load protocol"
    continue
  fi

  manifest=
  manifest_count=0
  for candidate in "$case_dir"/workdir/*.pkg.kio; do
    [ -f "$candidate" ] || continue
    manifest=$candidate
    manifest_count=$((manifest_count + 1))
  done
  if [ "$manifest_count" -ne 1 ]; then
    fail "$case_name: expected exactly one root package manifest, found $manifest_count"
    continue
  fi

  checked=$((checked + 1))
  bridge_entries=$(
    awk '
      /^[[:space:]]*bridge[[:space:]]*\{/ { in_bridge = 1; next }
      in_bridge && /^[[:space:]]*\}/ { exit }
      in_bridge {
        line = $0
        sub(/\/\/.*/, "", line)
        gsub(/[[:space:];]/, "", line)
        if (line != "") print line
      }
    ' "$manifest" | LC_ALL=C sort
  )
  if [ "$bridge_entries" != "$expected_bridge_entries" ]; then
    fail "$case_name: bridge surface must exactly match the testapi-dyn-load host interface; found:
$bridge_entries"
  fi
done

if [ "$checked" -eq 0 ]; then
  fail 'found no scripted dyn_load_prime host cases'
fi

if [ "$status" -eq 0 ]; then
  printf 'dyn-load-host-surface: OK (%s scripted hosts)\n' "$checked"
fi
exit "$status"
