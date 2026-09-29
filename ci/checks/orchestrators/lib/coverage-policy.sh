#!/bin/sh

: "${COVERAGE_POLICY_DIR:?caller must set COVERAGE_POLICY_DIR}"

: "${COVERAGE_ORCHESTRATOR_REGISTRY:=$COVERAGE_POLICY_DIR/orchestrator-registry.tsv}"
: "${COVERAGE_VERIFICATION_SCOPES:=$COVERAGE_POLICY_DIR/verification-scopes.tsv}"
: "${COVERAGE_NAMED_SETS:=$COVERAGE_POLICY_DIR/named-sets.tsv}"

coverage_policy_die() {
  printf 'error: %s\n' "$*" >&2
  exit 2
}

coverage_policy_validate_identifier() {
  case ${1-} in
    ''|*[!abcdefghijklmnopqrstuvwxyz0123456789-]*|-*) return 1 ;;
  esac
}

coverage_policy_validate_relative_path() {
  cpvrp_path=${1-}
  case $cpvrp_path in
    ''|/*|*/|*//*|*[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_./-]*)
      return 1
      ;;
  esac
  cpvrp_rest=$cpvrp_path
  while :; do
    case $cpvrp_rest in
      */*) cpvrp_part=${cpvrp_rest%%/*}; cpvrp_rest=${cpvrp_rest#*/} ;;
      *) cpvrp_part=$cpvrp_rest; cpvrp_rest= ;;
    esac
    case $cpvrp_part in ''|.|..) return 1 ;; esac
    [ -n "$cpvrp_rest" ] || break
  done
}

coverage_policy_validate_policy() {
  cpvp_value=${1-}
  case $cpvp_value in
    all|sample) return 0 ;;
    *[!0-9]*|'') coverage_policy_validate_identifier "$cpvp_value" ;;
    *[1-9]*) return 0 ;;
    *) return 1 ;;
  esac
}

coverage_policy_owner_for_scope() {
  awk -F '\t' -v scope="$1" \
    'NR > 1 && $1 == scope { print $2; found=1; exit }
     END { if (!found) exit 1 }' "$COVERAGE_VERIFICATION_SCOPES"
}

coverage_policy_parent_for_scope() {
  awk -F '\t' -v scope="$1" \
    'NR > 1 && $1 == scope { print $3; found=1; exit }
     END { if (!found) exit 1 }' "$COVERAGE_VERIFICATION_SCOPES"
}

coverage_policy_fixed_impl_for_scope() {
  awk -F '\t' -v scope="$1" \
    'NR > 1 && $1 == scope && $4 == "fixed-verification" {
       print $5; found=1; exit
     }
     END { if (!found) exit 1 }' "$COVERAGE_VERIFICATION_SCOPES"
}

coverage_policy_verification_scope_for_impl() {
  awk -F '\t' -v impl="$1" \
    'NR > 1 && $5 == impl && $4 == "fixed-verification" {
       print $1; found=1; exit
     }
     END { if (!found) exit 1 }' "$COVERAGE_VERIFICATION_SCOPES"
}

coverage_policy_root_for_owner() {
  awk -F '\t' -v owner="$1" \
    'NR > 1 && $1 == owner && $2 != "-" { print $2; found=1; exit }
     END { if (!found) exit 1 }' "$COVERAGE_ORCHESTRATOR_REGISTRY"
}

coverage_policy_owner_for_root() {
  awk -F '\t' -v scope="$1" \
    'NR > 1 && $2 == scope && $2 != "-" { print $1; found=1; exit }
     END { if (!found) exit 1 }' "$COVERAGE_ORCHESTRATOR_REGISTRY"
}

coverage_policy_classification_for_scope() {
  cpcfs_scope=$1
  if coverage_policy_owner_for_root "$cpcfs_scope" >/dev/null 2>&1; then
    printf 'root-corpus\n'
    return 0
  fi
  awk -F '\t' -v scope="$cpcfs_scope" \
    'NR > 1 && $1 == scope { print $4; found=1; exit }
     END { if (!found) exit 1 }' "$COVERAGE_VERIFICATION_SCOPES"
}

coverage_policy_named_set_registered() {
  awk -F '\t' -v scope="$1" -v policy="$2" \
    'NR > 1 && $1 == scope && $2 == policy { found=1; exit }
     END { exit !found }' "$COVERAGE_NAMED_SETS"
}

coverage_policy_named_set_file() {
  cpns_scope=$1
  cpns_policy=$2
  cpns_rel=$(awk -F '\t' -v scope="$cpns_scope" -v policy="$cpns_policy" \
    'NR > 1 && $1 == scope && $2 == policy {
       print $3; found=1; exit
     }
     END { if (!found) exit 1 }' "$COVERAGE_NAMED_SETS") || return 1
  coverage_policy_validate_relative_path "$cpns_rel" || return 1
  cpns_repo=${COVERAGE_POLICY_DIR%/ci/checks/orchestrators/lib}
  [ "$cpns_repo" != "$COVERAGE_POLICY_DIR" ] || return 1
  printf '%s/%s\n' "$cpns_repo" "$cpns_rel"
}

coverage_policy_validate_case_list() {
  cpvcl_file=$1
  cpvcl_label=$2
  if [ ! -f "$cpvcl_file" ] || [ -L "$cpvcl_file" ]; then
    coverage_policy_die "$cpvcl_label must name a regular non-symlink file"
  fi
  if ! LC_ALL=C awk '
    function safe(path, count, parts, i) {
      if (path == "" || path ~ /^\// || path ~ /\/$/ || path ~ /\/\//) return 0
      count = split(path, parts, "/")
      for (i = 1; i <= count; i++) {
        if (parts[i] == "." || parts[i] == ".." ||
            parts[i] !~ /^[A-Za-z0-9_][A-Za-z0-9_.-]*$/) return 0
      }
      return 1
    }
    !safe($0) || (NR > 1 && ("x" $0) <= ("x" previous)) { bad=1 }
    { previous=$0 }
    END { if (NR == 0 || bad) exit 1 }
  ' "$cpvcl_file"; then
    coverage_policy_die "$cpvcl_label must be non-empty, normalized, safe, sorted, and unique"
  fi
}

coverage_policy_validate_named_set_manifest() {
  cpvnsm_scope=$1
  cpvnsm_policy=$2
  cpvnsm_file=$(coverage_policy_named_set_file "$cpvnsm_scope" "$cpvnsm_policy") ||
    coverage_policy_die "unknown named case policy $cpvnsm_policy for scope $cpvnsm_scope"
  case $cpvnsm_file in
    /*) ;;
    *) coverage_policy_die "named set $cpvnsm_scope:$cpvnsm_policy did not resolve to an absolute manifest path" ;;
  esac
  coverage_policy_validate_case_list "$cpvnsm_file" \
    "named set $cpvnsm_scope:$cpvnsm_policy manifest"
}

coverage_policy_validate_named_set() {
  cpvns_scope=$1
  cpvns_policy=$2
  cpvns_canonical=$3
  cpvns_cases_dir=$4
  cpvns_tab=$(printf '\t')
  cpvns_row=$(awk -F '\t' -v scope="$cpvns_scope" -v policy="$cpvns_policy" \
    'NR > 1 && $1 == scope && $2 == policy { print; found=1; exit }
     END { if (!found) exit 1 }' "$COVERAGE_NAMED_SETS") ||
    coverage_policy_die "unknown named case policy $cpvns_policy for scope $cpvns_scope"
  IFS=$cpvns_tab read -r cpvns_row_scope cpvns_row_policy _cpvns_manifest \
    cpvns_class cpvns_min cpvns_overlap <<EOF
$cpvns_row
EOF
  if [ "$cpvns_row_scope" != "$cpvns_scope" ] ||
     [ "$cpvns_row_policy" != "$cpvns_policy" ]; then
    coverage_policy_die \
      "invalid named-set registry row for $cpvns_scope:$cpvns_policy"
  fi
  coverage_policy_validate_named_set_manifest "$cpvns_scope" "$cpvns_policy"
  coverage_policy_validate_case_list "$cpvns_canonical" \
    "named set $cpvns_scope:$cpvns_policy canonical cohort"
  [ -d "$cpvns_cases_dir" ] ||
    coverage_policy_die "named set $cpvns_scope:$cpvns_policy cases directory is absent"
  cpvns_file=$(coverage_policy_named_set_file "$cpvns_scope" "$cpvns_policy")
  cpvns_witnesses=0
  while IFS= read -r cpvns_case; do
    if ! awk -v member="$cpvns_case" '$0 == member { found=1; exit }
        END { exit !found }' "$cpvns_canonical"; then
      coverage_policy_die "named set $cpvns_scope:$cpvns_policy contains member outside its canonical scope: $cpvns_case"
    fi
    cpvns_case_dir=$cpvns_cases_dir/$cpvns_case
    if [ "$cpvns_overlap" = reject ] && [ -f "$cpvns_case_dir/KNOWN_FAILING" ]; then
      coverage_policy_die "$cpvns_class $cpvns_scope:$cpvns_policy overlaps KNOWN_FAILING: $cpvns_case"
    fi
    cpvns_exit=$(sed -n '1p' "$cpvns_case_dir/expected.exit" 2>/dev/null || :)
    if [ "$cpvns_exit" = 0 ] && [ ! -f "$cpvns_case_dir/KNOWN_FAILING" ]; then
      cpvns_witnesses=$((cpvns_witnesses + 1))
    fi
  done <"$cpvns_file"
  [ "$cpvns_witnesses" -ge "$cpvns_min" ] ||
    coverage_policy_die "$cpvns_class $cpvns_scope:$cpvns_policy has fewer than $cpvns_min successful non-KNOWN_FAILING witnesses"
}

coverage_policy_validate_owner_fixed_impls() {
  cpvofi_owner=$1
  cpvofi_defined=$2
  if [ ! -f "$cpvofi_defined" ] || [ -L "$cpvofi_defined" ]; then
    coverage_policy_die \
      "defined implementation inventory for $cpvofi_owner must be a regular non-symlink file"
  fi
  while IFS=$(printf '\t') read -r cpvofi_scope cpvofi_row_owner _cpvofi_parent \
    _cpvofi_class cpvofi_impl _cpvofi_toolchain; do
    [ "$cpvofi_scope" = scope ] && continue
    [ "$cpvofi_row_owner" = "$cpvofi_owner" ] || continue
    if ! awk -v impl="$cpvofi_impl" '$0 == impl { found=1; exit }
        END { exit !found }' "$cpvofi_defined"; then
      coverage_policy_die "verification scope $cpvofi_scope names fixed implementation not defined by owner $cpvofi_owner: $cpvofi_impl"
    fi
  done <"$COVERAGE_VERIFICATION_SCOPES"
}

coverage_policy_validate_registry() (
  cpvr_dir=$1
  LC_ALL=C
  export LC_ALL
  cpvr_repo=${COVERAGE_POLICY_DIR%/ci/checks/orchestrators/lib}
  [ "$cpvr_repo" != "$COVERAGE_POLICY_DIR" ] ||
    coverage_policy_die 'cannot derive repository root for registry scratch'
  cpvr_parent=${TMPDIR:-$cpvr_repo/target}
  mkdir -p "$cpvr_parent" ||
    coverage_policy_die "cannot create registry scratch parent: $cpvr_parent"
  cpvr_tmp=$(mktemp -d "$cpvr_parent/coverage-registry.XXXXXX") || exit 2
  trap 'rm -rf "$cpvr_tmp"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  cpvr_tab=$(printf '\t')

  for cpvr_table in "$COVERAGE_ORCHESTRATOR_REGISTRY" \
    "$COVERAGE_VERIFICATION_SCOPES" "$COVERAGE_NAMED_SETS"; do
    if [ ! -f "$cpvr_table" ] || [ -L "$cpvr_table" ]; then
      coverage_policy_die \
        "coverage registry must be a regular non-symlink file: $cpvr_table"
    fi
  done

  IFS= read -r cpvr_header <"$COVERAGE_ORCHESTRATOR_REGISTRY" || cpvr_header=
  [ "$cpvr_header" = "orchestrator${cpvr_tab}root_scope${cpvr_tab}declaration" ] ||
    coverage_policy_die 'invalid orchestrator-registry header'
  IFS= read -r cpvr_header <"$COVERAGE_VERIFICATION_SCOPES" || cpvr_header=
  [ "$cpvr_header" = "scope${cpvr_tab}owner${cpvr_tab}parent${cpvr_tab}classification${cpvr_tab}fixed_impl${cpvr_tab}toolchain_class" ] ||
    coverage_policy_die 'invalid verification-scopes header'
  IFS= read -r cpvr_header <"$COVERAGE_NAMED_SETS" || cpvr_header=
  [ "$cpvr_header" = "scope${cpvr_tab}policy${cpvr_tab}manifest${cpvr_tab}classification${cpvr_tab}minimum_non_known_failing_success${cpvr_tab}known_failing_overlap" ] ||
    coverage_policy_die 'invalid named-sets header'

  : >"$cpvr_tmp/discovered.unsorted"
  # Match ci/all.sh's direct regular-file owner set, including dot-prefixed
  # names but excluding symlinks. The three guarded patterns are portable
  # POSIX shell enumeration; an unmatched pattern simply fails the tests.
  for cpvr_path in "$cpvr_dir"/* "$cpvr_dir"/.[!.]* "$cpvr_dir"/..?*
  do
    if [ ! -f "$cpvr_path" ] || [ -L "$cpvr_path" ]; then
      continue
    fi
    cpvr_name=${cpvr_path##*/}
    case $cpvr_name in
      *.sh) printf '%s\n' "$cpvr_name" >>"$cpvr_tmp/discovered.unsorted" ;;
    esac
  done
  LC_ALL=C sort "$cpvr_tmp/discovered.unsorted" >"$cpvr_tmp/discovered"
  if ! awk -F '\t' 'NR > 1 {
      if (NF != 3 || $1 !~ /^[A-Za-z0-9][A-Za-z0-9_-]*[.]sh$/ ||
          ($2 != "-" && $2 !~ /^[a-z0-9][a-z0-9-]*$/) ||
          $3 != "COMPLETE" || seen_owner[$1]++ ||
          ($2 != "-" && seen_root[$2]++)) exit 2
      print $1
    }' "$COVERAGE_ORCHESTRATOR_REGISTRY" >"$cpvr_tmp/registered"; then
    coverage_policy_die 'invalid orchestrator registry'
  fi
  LC_ALL=C sort -u "$cpvr_tmp/registered" >"$cpvr_tmp/registered.sorted"
  cmp -s "$cpvr_tmp/registered" "$cpvr_tmp/registered.sorted" ||
    coverage_policy_die 'orchestrator registry must be sorted by unique owner'
  cmp -s "$cpvr_tmp/discovered" "$cpvr_tmp/registered" ||
    coverage_policy_die 'orchestrator registry does not match discovered owners'

  if ! awk -F '\t' '
    NR == FNR && FNR > 1 { owner[$1]=$2; if ($2 != "-") root[$2]=1; next }
    FNR > 1 {
      if (NF != 6 || $1 !~ /^[a-z0-9][a-z0-9-]*$/ ||
          seen_scope[$1]++ || ($1 in root) || seen_impl[$5]++ ||
          $4 != "fixed-verification" || $5 !~ /^[^[:space:]]+$/ ||
          $6 != "core" || !($2 in owner) || owner[$2] != $3 || $3 == "-")
        exit 2
    }' "$COVERAGE_ORCHESTRATOR_REGISTRY" "$COVERAGE_VERIFICATION_SCOPES"; then
    coverage_policy_die 'invalid verification-scope registry'
  fi

  awk -F '\t' 'NR > 1 && $2 != "-" { print $2 }' \
    "$COVERAGE_ORCHESTRATOR_REGISTRY" >"$cpvr_tmp/scopes"
  awk -F '\t' 'NR > 1 { print $1 }' "$COVERAGE_VERIFICATION_SCOPES" \
    >>"$cpvr_tmp/scopes"
  LC_ALL=C sort -u "$cpvr_tmp/scopes" >"$cpvr_tmp/scopes.sorted"
  if ! awk -F '\t' '
    NR == FNR { scope[$1]=1; next }
    FNR > 1 {
      key=$1 SUBSEP $2
      if (NF != 6 || !($1 in scope) || $2 !~ /^[a-z0-9][a-z0-9-]*$/ ||
          seen[key]++ || seen_manifest[$3]++ ||
          $4 != "success-smoke" || $5 !~ /^[1-9][0-9]*$/ ||
          $6 != "reject") exit 2
    }' "$cpvr_tmp/scopes.sorted" "$COVERAGE_NAMED_SETS"; then
    coverage_policy_die 'invalid named-set registry'
  fi
  while IFS=$cpvr_tab read -r cpvr_scope cpvr_policy cpvr_manifest \
    _cpvr_class _cpvr_min _cpvr_overlap; do
    [ "$cpvr_scope" = scope ] && continue
    coverage_policy_validate_relative_path "$cpvr_manifest" ||
      coverage_policy_die "named set $cpvr_scope:$cpvr_policy has a non-normalized manifest path"
    coverage_policy_validate_named_set_manifest "$cpvr_scope" "$cpvr_policy"
  done <"$COVERAGE_NAMED_SETS"
)
