#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/kio-version-check.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
fixture="$scratch/repo with spaces"
check="$fixture/ci/checks/repo-lint/version-check.sh"
parser="$fixture/tools/tree-sitter-kio/src/parser.c"
mkdir -p "$(dirname -- "$check")" "$(dirname -- "$parser")"
cp "$SCRIPT_DIR/version-check.sh" "$check"

# Copy only the check's input inventory; parser metadata needs no grammar body.
awk '
  { separator = index($0, "${TAB}") }
  separator > 0 {
    file = substr($0, 1, separator - 1)
    if (file ~ /^[.[:alnum:]][^[:space:]]*$/) print file
  }
' "$check" >"$scratch/inputs"
printf '%s\n' kio-rs/fuzz/Cargo.toml >>"$scratch/inputs"
while IFS= read -r file; do
  case "$file" in tools/tree-sitter-kio/src/parser.c) continue ;; esac
  mkdir -p "$(dirname -- "$fixture/$file")"
  cp "$REPO_ROOT/$file" "$fixture/$file"
done <"$scratch/inputs"

version=$(sed -n '/^version *= *"/{s/.*"\(.*\)".*/\1/p;q;}' "$fixture/kio-rs/Cargo.toml")
major=${version%%.*}
remaining=${version#*.}
minor=${remaining%%.*}
patch=${remaining#*.}
write_metadata() {
  printf '    .metadata = {\n      .major_version = %s,\n      .minor_version = %s,\n      .patch_version = %s,\n    },\n' "$1" "$2" "$3" >"$parser"
}

write_metadata "$major" "$minor" "$patch"
sh "$check" >"$scratch/positive.log" 2>&1
grep -F "tools/tree-sitter-kio/src/parser.c" "$scratch/positive.log" | grep -F "$version" >/dev/null

for component in major minor patch; do
  case "$component" in
    major) write_metadata "$((major + 1))" "$minor" "$patch" ;;
    minor) write_metadata "$major" "$((minor + 1))" "$patch" ;;
    patch) write_metadata "$major" "$minor" "$((patch + 1))" ;;
  esac
  if sh "$check" >"$scratch/mismatch.log" 2>&1; then
    printf 'version-check-selftest: accepted mismatched parser %s version\n' "$component" >&2
    exit 1
  fi
  grep -F 'version-check: mismatch' "$scratch/mismatch.log" | grep -F 'tools/tree-sitter-kio/src/parser.c' >/dev/null
done

printf 'version-check-selftest: pass (matching parser metadata; major/minor/patch mismatch rejected)\n'
