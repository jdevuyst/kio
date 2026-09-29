#!/bin/sh
#
# Verify repo-local agent skills have loadable YAML frontmatter.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

field_count() {
  file=$1
  field=$2
  awk -v field="$field" '
    NR > 1 && $0 == "---" { exit }
    NR > 1 && index($0, field ":") == 1 { count++ }
    END { print count + 0 }
  ' "$file"
}

field_value() {
  file=$1
  field=$2
  awk -v field="$field" '
    NR > 1 && $0 == "---" { exit }
    NR > 1 && index($0, field ":") == 1 {
      value = substr($0, length(field) + 2)
      sub(/^[[:space:]]*/, "", value)
      print value
      exit
    }
  ' "$file"
}

failures=0
skill_files=$(find ai/skills -mindepth 2 -maxdepth 2 -name SKILL.md -type f | sort)

if [ -z "$skill_files" ]; then
  printf 'error: no skill files found under ai/skills\n' >&2
  exit 1
fi

for file in $skill_files; do
  first_line=$(sed -n '1p' "$file")
  if [ "$first_line" != "---" ]; then
    printf '%s: missing YAML frontmatter delimited by --- (line 1 must be exactly ---)\n' "$file" >&2
    failures=1
    continue
  fi

  closing_line=$(awk 'NR > 1 && $0 == "---" { print NR; exit }' "$file")
  if [ -z "$closing_line" ]; then
    printf '%s: missing YAML frontmatter delimited by --- (missing closing ---)\n' "$file" >&2
    failures=1
    continue
  fi

  for field in name description allowed-tools; do
    count=$(field_count "$file" "$field")
    case "$count" in
      0)
        printf '%s: frontmatter missing required field: %s\n' "$file" "$field" >&2
        failures=1
        ;;
      1) ;;
      *)
        printf '%s: frontmatter field appears more than once: %s\n' "$file" "$field" >&2
        failures=1
        ;;
    esac
  done

  name=$(field_value "$file" name)
  if [ -z "$name" ]; then
    printf '%s: frontmatter field is empty: name\n' "$file" >&2
    failures=1
  fi

  description=$(field_value "$file" description)
  if [ -z "$description" ]; then
    printf '%s: frontmatter field is empty: description\n' "$file" >&2
    failures=1
  fi

  allowed_tools=$(field_value "$file" allowed-tools)
  if [ -z "$allowed_tools" ]; then
    printf '%s: frontmatter field is empty: allowed-tools\n' "$file" >&2
    failures=1
  fi

  dir_name=$(basename -- "$(dirname -- "$file")")
  if [ -n "$name" ] && [ "$name" != "$dir_name" ]; then
    printf '%s: frontmatter name %s does not match directory %s\n' "$file" "$name" "$dir_name" >&2
    failures=1
  fi
done

if [ "$failures" -ne 0 ]; then
  exit 1
fi
