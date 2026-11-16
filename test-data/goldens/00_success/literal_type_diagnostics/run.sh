#!/bin/sh
# Contextual inference, explicit annotations, and literal admission are compiler-phase contracts.
# Each isolated fixture pins a diagnostic or explicit annotation/equivalence
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

source_dir=$case_dir/fixtures

check_case() {
  expected=$1
  name=$2
  cp "$source_dir/$name/main.kio" "$work/main.kio"
  status=0
  (cd "$work" &&
    "$KIO_BIN" test
  ) >"$work/stdout" 2>"$work/stderr" || status=$?
  if [ "$status" -ne "$expected" ]; then
    cat "$work/stdout" "$work/stderr" >&2
    printf '%s: expected exit %s, got %s\n' "$name" "$expected" "$status" >&2
    exit 1
  fi
  if [ "$#" -ge 3 ] && ! grep -F "$3" "$work/stderr" >/dev/null; then
    cat "$work/stderr" >&2
    printf '%s: missing expected diagnostic: %s\n' "$name" "$3" >&2
    exit 1
  fi
  printf '%s: %s\n' "$name" "$status"
}

for name in expected_types explicit_types anchored_siblings import_before import_extended imported_roles_before imported_roles_after imports_all_categories applied_aliases multiple_integer_carriers multiple_float_carriers; do
  check_case 0 "$name"
done
check_case 13 missing_builtin_type
for name in missing_integer missing_float missing_string missing_bool late_host late_alias unanchored_import; do
  check_case 14 "$name" "cannot determine"
done
for name in missing_bool_intrinsic mixed_integer_first mixed_float_first; do
  check_case 14 "$name"
done
check_case 14 explicit_float "decimal point"
check_case 14 expected_float "decimal point"
