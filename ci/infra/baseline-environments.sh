#!/bin/sh
# Baseline identities refer to provisioning environments, never version pins.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
table=$script_dir/baseline-environments.tsv
case "${1:-}" in
  requirements)
    [ "$#" -eq 3 ] || { printf 'usage: baseline-environments.sh requirements LANGUAGES sample|current\n' >&2; exit 2; }
    case "$3" in sample|current) ;; *) printf 'error: unknown baseline environment policy: %s\n' "$3" >&2; exit 2 ;; esac
    awk -v languages="$2" -v mode="$3" '
      BEGIN { n = split(languages, requested, ","); for (i = 1; i <= n; i++) wanted[requested[i]] = 1 }
      /^#/ || NF == 0 { next }
      $1 in wanted {
        found[$1] = 1
        if (mode == "sample" && $5 != "locked-quickjs" && !seen[$1, $3]++) print $1 "\t" $3
        if (!seen[$1, $4]++) print $1 "\t" $4
      }
      END { for (language in wanted) if (!(language in found)) exit 2 }
    ' "$table" || { printf 'error: unknown target language in baseline requirements: %s\n' "$2" >&2; exit 2; }
    ;;
  resolve)
    [ "$#" -eq 4 ] || { printf 'usage: baseline-environments.sh resolve BACKEND BASELINE minimum|current-tested|both\n' >&2; exit 2; }
    awk -v backend="$2" -v baseline="$3" -v endpoint="$4" '
      /^#/ || NF == 0 { next }
      $1 == backend && ($2 == baseline || baseline == "latest") {
        found = 1; minimum = $3; current = $4
      }
      END {
        if (!found) exit 2
        if (endpoint == "minimum") print minimum
        else if (endpoint == "current-tested") print current
        else if (endpoint == "both") { print minimum; if (current != minimum) print current }
        else exit 2
      }
    ' "$table" || { printf 'error: unknown baseline or endpoint: %s/%s/%s\n' "$2" "$3" "$4" >&2; exit 2; }
    ;;
  expand)
    [ "$#" -eq 8 ] || { printf 'usage: baseline-environments.sh expand ROWS POLICIES MODE SEED SCOPE CASES_DIR OUTPUT\n' >&2; exit 2; }
    LC_ALL=C awk -F '\t' -v table="$table" -v policies="$3" \
      -v mode="$4" -v seed="$5" -v scope="$6" -v cases_dir="$7" \
      -f "$script_dir/baseline-plan.awk" "$table" "$3" "$2" >"$8"
    ;;
  *) printf 'usage: baseline-environments.sh resolve|requirements|expand ...\n' >&2; exit 2 ;;
esac
