#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
CASTLE_TESTS_SH=${CASTLE_TESTS_SH:-"$REPO_ROOT/ci/checks/orchestrators/castle-tests.sh"}
COMMON_SH=$REPO_ROOT/ci/checks/orchestrators/lib/common.sh

# Check the whole script even when a help/validation path exits before its end.
sh -n "$CASTLE_TESTS_SH"

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/castle-contract-selftest.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

awk '/^validate_castle_contract\(\) \{/ { copy = 1 }
     copy { print }
     copy && /^\}/ { exit }' "$CASTLE_TESTS_SH" >"$scratch/validate.sh"
test -s "$scratch/validate.sh"

corpus=$scratch/repo/test-data/castles
castle=$corpus/example
mkdir -p "$castle/workdir/library"
: >"$corpus/README.md"
: >"$castle/README.md"
: >"$castle/run.args"
: >"$castle/expected.stdout"
: >"$castle/expected.stderr"
printf '0\n' >"$castle/expected.exit"
printf 'module main;\n' >"$castle/workdir/main.kio"
: >"$castle/workdir/library.dep.kio"
printf 'host fn read_ascii_line() -> ();\n' >"$castle/workdir/library/main.kio"

validate() {
  REPO_ROOT="$scratch/repo" CASTLES_DIR="$corpus" sh -eu -c '
    . "$1"
    . "$2"
    validate_castle_contract
  ' sh "$COMMON_SH" "$scratch/validate.sh"
}

# A dependency may export an unused reader without its consumer reading stdin.
validate >"$scratch/unused-reader.log" 2>&1 || {
  cat "$scratch/unused-reader.log" >&2
  exit 1
}

printf 'host fn read_ascii_line() -> ();\n' >>"$castle/workdir/main.kio"
if validate >"$scratch/missing-input.log" 2>&1; then
  printf 'castle-contract-selftest: accepted a reader without input.stdin\n' >&2
  exit 1
fi
grep -q 'declares read_ascii_line() but has no input.stdin fixture' \
  "$scratch/missing-input.log"

: >"$castle/input.stdin"
validate >"$scratch/with-input.log" 2>&1 || {
  cat "$scratch/with-input.log" >&2
  exit 1
}

# A help path must not hide a parse error later in the script.
mkdir -p "$scratch/invalid/lib"
cp "$CASTLE_TESTS_SH" "$scratch/invalid/castle-tests.sh"
cp "$COMMON_SH" "$scratch/invalid/lib/common.sh"
printf '\n;;\n' >>"$scratch/invalid/castle-tests.sh"
if sh "$scratch/invalid/castle-tests.sh" --help >"$scratch/syntax.log" 2>&1; then
  printf 'castle-contract-selftest: accepted a script with invalid syntax\n' >&2
  exit 1
fi
grep -qi 'syntax error' "$scratch/syntax.log"

printf 'castle-contract-selftest: PASS (dependency reader, stdin, syntax failure)\n'
