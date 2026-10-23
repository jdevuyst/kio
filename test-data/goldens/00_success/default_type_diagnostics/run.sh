#!/bin/sh
# Default selection and strict literal admission are compiler-phase contracts.
# Each isolated fixture has one error, or explicit expected-type/equivalence
# controls. The routing target does not claim generated-backend behavior.
set -eu

case_dir=$(pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
(cd workdir &&
  "$KIO_BIN" test
) >/dev/null
"$KIO_BIN" fmt --check "$case_dir/fixtures" >/dev/null
cp "$case_dir/fixtures/numbers.kio" "$work/numbers.kio"
cp "$case_dir/fixtures/provider.kio" "$work/provider.kio"

check_case() {
  expected=$1
  name=$2
  cp "$case_dir/fixtures/$name/main.kio" "$work/main.kio"
  status=0
  (cd "$work" &&
    "$KIO_BIN" test
  ) >"$work/stdout" 2>"$work/stderr" || status=$?
  if [ "$status" -ne "$expected" ]; then
    cat "$work/stdout" "$work/stderr" >&2
    printf '%s: expected exit %s, got %s\n' "$name" "$expected" "$status" >&2
    exit 1
  fi
  printf '%s: %s\n' "$name" "$status"
}

for name in expected_types same_category import_before import_extended imported_defaults_before imported_defaults_after imports_all_categories applied_aliases; do
  check_case 0 "$name"
done
check_case 13 missing_builtin_type
for name in conflict_integer conflict_float missing_integer missing_float missing_string missing_bool_intrinsic mixed_integer_first mixed_float_first explicit_float expected_float late_host late_alias ordinary_import_nonpropagation; do
  check_case 14 "$name"
done
