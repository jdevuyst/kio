#!/bin/sh
#
# Layer-(3b) mutation-testing signal for kio-rs core passes.
#
# Runs cargo-mutants over the kio-rs semantic core — typer, elaborator
# registry, substitute pass, normalizer, optimizer, and Prime path; see the
# charter above MUTATION_TARGETS — with a wall-clock cap. A surviving
# mutant is a place where the test corpus is too weak to catch a
# behavioral change — direction-setting signal for what to harden
# next, not a gate.
#
# The wall-clock budget is bounded; cargo-mutants randomizes mutant
# order via --shuffle so each run samples differently. Over weeks
# the picture fills in. A surviving mutant from this week's report
# should be reproduced locally (sh reports/mutation.sh --timeout=0
# to run without cap), then either: (a) a regression test added
# that would have caught the mutation, or (b) the mutation marked
# as intentionally-equivalent in the cargo-mutants config
# (kio-rs/.cargo/mutants.toml — entries are anchored on function
# name plus exact rewrite, each with its equivalence argument) if
# the source admits multiple-equivalent rewrites for the same
# semantics. One survivor class is expected and deliberately not
# excluded: mutants inside `#[cfg(not(feature = "parallel"))]` twin
# bodies always survive under the default-features build (the cfg
# strips the code, so no test can observe the mutation); read a
# survivor's line number against those blocks before triaging it
# as a gap.
#
# Default-report semantics: cargo-mutants exit codes 0 / 2 / 3 are all
# expected outcomes (clean / survivors / timeouts respectively) and map to
# script exit 0. The deterministic validator instead uses exit 3 to keep a
# campaign open when test-gap or unresolved triage remains. Other exit codes
# identify real harness or artifact failures.
#
# Pre-req: `cargo-mutants` (install with
# `sh ci/impl-toolchain.sh install-report-tools`).
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

# The charter — what this harness mutates, and why. Mutate the
# SEMANTIC CORE: the passes and algorithms where a subtle behavioral
# change is a soundness or normalization bug that the other test
# layers — fuzz (reports/fuzz.sh), the golden corpus, the Prime
# verifier, and the backend coverage matrix
# (audit-backend-completeness) — would not reliably catch. In scope
# under that principle:
#
#   - the typer (src/pass/typecheck_core/** + typecheck_full.rs + its
#     production submodules) and the elaborator registry — the
#     type-soundness core;
#   - the substitute pass (src/pass/substitute/**) — the
#     Lowered → Prime elaboration-baking boundary;
#   - the normalizer (src/normalization.rs) — the reducer behind `equiv`,
#     `:normalize`, and compile-time elaborator evaluation; the
#     strong-normalization guarantee lives here;
#   - the backend-neutral optimizer (src/pass/optimize.rs) — its catalog is
#     behavior-preserving performance machinery, so private firing must not
#     silently disappear while behavioral output remains unchanged;
#   - the Prime path (src/prime/**) — the standalone Kio'-only typer
#     (used by the kio-prime binary AND by the full pipeline to
#     validate substituted Prime) plus the Surface → Prime walk that
#     enforces the no-surface-forms-in-Kio' boundary; its golden
#     coverage alone is thin.
#
# Deliberately out of scope, each owned by another layer: lexer +
# parser (reports/fuzz.sh owns the front end); desugaring —
# src/pass/desugar/, label_elab/, op_fold.rs (the label/desugar
# goldens across backends plus the Prime verifier own surface-form
# removal); name resolution — src/pass/resolve.rs (binding changes
# surface as type errors or changed output across the golden corpus);
# backends — src/backends/** (the golden corpus and
# audit-backend-completeness own them; mutating them here would drown
# the semantic core).
#
# The audit-mutation skill's scope-review step diffs this list against
# the kio-rs module inventory each run; change the list only together
# with the charter argument above.
#
# Glob mechanics: these follow cargo-mutants' `--file` matching — a
# glob containing a slash is matched against the whole path, so a
# directory must end in `/**` to reach the files inside it; a bare
# directory path matches nothing at all. This is the single source of
# truth for the harness's reach: the dependency-free scope self-test checks
# every production source against it, and smoke-reports.sh additionally
# validates one real combined cargo-mutants manifest when that optional tool is
# installed. Thus a renamed module, a dropped `/**`, or a candidate exclusion
# cannot silently shrink coverage (as happened when the built-in elaborator
# engines were deleted and the directory globs lost their `/**` — the harness
# quietly mutated only typecheck_full.rs while advertising four passes).
MUTATION_TARGETS='src/pass/typecheck_core/**
src/pass/typecheck_full.rs
src/pass/typecheck_full/**
src/pass/elaborator_registry.rs
src/pass/substitute/**
src/normalization.rs
src/pass/optimize.rs
src/prime/**'

# Exact source files outside a claimed directory tree. Source trees are
# discovered below, so a new production submodule automatically joins the
# fail-closed scope check rather than relying on this list being remembered.
MUTATION_PRODUCTION_FILES='src/pass/typecheck_full.rs
src/pass/elaborator_registry.rs
src/normalization.rs
src/pass/optimize.rs'

MUTATION_PRODUCTION_DIRS='src/pass/typecheck_core
src/pass/typecheck_full
src/pass/substitute
src/prime'

# This file is included only by typecheck_full.rs's #[cfg(test)] module. It is
# deliberately not evidence that the adjacent production publication module
# is selected.
MUTATION_TEST_ONLY_SOURCES='src/pass/typecheck_full/annotation_plan_public_tests.rs'

# These names are checked against the actual optimizer catalog below and then
# against cargo-mutants' real candidate manifest by smoke-reports.sh. Keeping
# them explicit makes a candidate exclusion or a catalog edit fail closed.
MUTATION_OPTIMIZER_FIRING_SITES='project_after_tuple
compose_recovered_tail_access
let_bound_tuple_projection
inject_then_match
absurd_propagate
match_fusion
let_fuse
beta_reduce_unary_iife
dead_code_elimination
newtype_identity_elision
constant_fold'

TIMEOUT=900  # 15 min total wall-clock cap; 0 disables.
TIMEOUT_SET=0
MODE=broad
MANIFEST_DIR=
OUTPUT_DIR=
SHARD_SPEC=
SHARD_OUTPUTS=
CLEANUP_DIR=
EQUIV_SOURCE=src/normalization.rs
EQUIV_SOURCE_REPO=kio-rs/src/normalization.rs
EQUIV_CONFIG_REPO=kio-rs/.cargo/mutants.toml

cleanup() {
  if [ -n "$CLEANUP_DIR" ] && [ -e "$CLEANUP_DIR" ]; then
    rm -rf "$CLEANUP_DIR"
  fi
}

trap cleanup 0
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

die() {
  printf 'error: %s\n' "$*" >&2
  exit 2
}

is_test_only_mutation_source() {
  itoms_source=$1
  itoms_oldifs=$IFS
  set -f
  IFS='
'
  for itoms_excluded in $MUTATION_TEST_ONLY_SOURCES; do
    if [ "$itoms_source" = "$itoms_excluded" ]; then
      set +f
      IFS=$itoms_oldifs
      return 0
    fi
  done
  set +f
  IFS=$itoms_oldifs
  return 1
}

validate_mutation_source_roots() {
  vmsr_bad=0
  vmsr_oldifs=$IFS
  set -f
  IFS='
'
  for vmsr_file in $MUTATION_PRODUCTION_FILES; do
    if [ ! -f "$REPO_ROOT/kio-rs/$vmsr_file" ]; then
      printf 'error: claimed mutation source is missing: %s\n' "$vmsr_file" >&2
      vmsr_bad=1
    fi
  done
  for vmsr_dir in $MUTATION_PRODUCTION_DIRS; do
    if [ ! -d "$REPO_ROOT/kio-rs/$vmsr_dir" ]; then
      printf 'error: claimed mutation source directory is missing: %s\n' \
        "$vmsr_dir" >&2
      vmsr_bad=1
    fi
  done
  for vmsr_excluded in $MUTATION_TEST_ONLY_SOURCES; do
    if [ ! -f "$REPO_ROOT/kio-rs/$vmsr_excluded" ]; then
      printf 'error: claimed test-only mutation exclusion is missing: %s\n' \
        "$vmsr_excluded" >&2
      vmsr_bad=1
    fi
  done
  set +f
  IFS=$vmsr_oldifs
  [ "$vmsr_bad" -eq 0 ]
}

print_mutation_production_sources_unchecked() {
  pmps_sources=$(
    cd "$REPO_ROOT/kio-rs" || exit 1
    pmps_oldifs=$IFS
    set -f
    IFS='
'
    for pmps_file in $MUTATION_PRODUCTION_FILES; do
      printf '%s\n' "$pmps_file"
    done
    for pmps_dir in $MUTATION_PRODUCTION_DIRS; do
      find "$pmps_dir" -type f -name '*.rs' -print || exit 1
    done
    set +f
    IFS=$pmps_oldifs
  ) || return 1
  printf '%s\n' "$pmps_sources" | LC_ALL=C sort -u | while IFS= read -r pmps_source; do
    if ! is_test_only_mutation_source "$pmps_source"; then
      printf '%s\n' "$pmps_source"
    fi
  done
}

print_mutation_production_sources() {
  validate_mutation_source_roots || return 1
  print_mutation_production_sources_unchecked
}

mutation_source_is_targeted() {
  msit_source=$1
  msit_oldifs=$IFS
  set -f
  IFS='
'
  for msit_target in $MUTATION_TARGETS; do
    case "$msit_target" in
      */'**')
        msit_prefix=${msit_target%/**}
        case "$msit_source" in
          "$msit_prefix"/*)
            set +f
            IFS=$msit_oldifs
            return 0
            ;;
        esac
        ;;
      *)
        if [ "$msit_source" = "$msit_target" ]; then
          set +f
          IFS=$msit_oldifs
          return 0
        fi
        ;;
    esac
  done
  set +f
  IFS=$msit_oldifs
  return 1
}

print_optimizer_firing_sites() {
  pofs_oldifs=$IFS
  set -f
  IFS='
'
  for pofs_name in $MUTATION_OPTIMIZER_FIRING_SITES; do
    printf 'src/pass/optimize.rs\t%s\n' "$pofs_name"
  done
  set +f
  IFS=$pofs_oldifs
}

optimizer_catalog_firing_sites() {
  sed -n '/^fn optimize_expr_in_mode(/,/^}/p' \
    "$REPO_ROOT/kio-rs/src/pass/optimize.rs" |
    sed -n '/&mut changed/ s/^[[:space:]]*e = \([a-z_][a-z_]*\)(.*/\1/p' |
    LC_ALL=C sort -u
}

validate_declared_mutation_scope() {
  vdms_bad=0
  validate_mutation_source_roots || return 1
  if ! printf '%s\n' "$MUTATION_TARGETS" | awk '
      NF == 0 || index($0, "\t") || index($0, "\r") || seen[$0]++ { exit 1 }
    '; then
    printf 'error: mutation targets are empty, duplicated, or malformed\n' >&2
    vdms_bad=1
  fi

  vdms_production_sources=$(print_mutation_production_sources_unchecked) || return 1
  while IFS= read -r vdms_source; do
    if [ ! -f "$REPO_ROOT/kio-rs/$vdms_source" ]; then
      printf 'error: claimed mutation source is missing: %s\n' "$vdms_source" >&2
      vdms_bad=1
    elif ! mutation_source_is_targeted "$vdms_source"; then
      printf 'error: claimed production source is outside MUTATION_TARGETS: %s\n' \
        "$vdms_source" >&2
      vdms_bad=1
    fi
  done <<EOF
$vdms_production_sources
EOF

  vdms_expected=$(printf '%s\n' "$MUTATION_OPTIMIZER_FIRING_SITES" | LC_ALL=C sort -u)
  vdms_actual=$(optimizer_catalog_firing_sites)
  if [ "$vdms_expected" != "$vdms_actual" ]; then
    printf 'error: optimizer firing inventory disagrees with optimize_expr_in_mode\n' >&2
    printf 'declared:\n%s\nactual:\n%s\n' "$vdms_expected" "$vdms_actual" >&2
    vdms_bad=1
  fi

  while IFS='	' read -r vdms_source vdms_name; do
    if [ -z "$vdms_source" ] || [ -z "$vdms_name" ]; then
      printf 'error: malformed required mutation firing site\n' >&2
      vdms_bad=1
    elif ! mutation_source_is_targeted "$vdms_source"; then
      printf 'error: required firing site is outside MUTATION_TARGETS: %s::%s\n' \
        "$vdms_source" "$vdms_name" >&2
      vdms_bad=1
    elif ! grep -Eq "fn[[:space:]]+${vdms_name}[[:space:]]*\\(" \
        "$REPO_ROOT/kio-rs/$vdms_source"; then
      printf 'error: required mutation firing function is missing: %s::%s\n' \
        "$vdms_source" "$vdms_name" >&2
      vdms_bad=1
    fi
  done <<EOF
$(print_optimizer_firing_sites)
EOF

  [ "$vdms_bad" -eq 0 ] || return 1
  vdms_sources=$(printf '%s\n' "$vdms_production_sources" | awk 'END { print NR + 0 }')
  vdms_sites=$(print_optimizer_firing_sites | awk 'END { print NR + 0 }')
  printf 'mutation: declared scope covers %s production sources and %s optimizer firing sites\n' \
    "$vdms_sources" "$vdms_sites"
}

target_candidate_count() {
  tcc_manifest=$1
  tcc_target=$2
  case "$tcc_target" in
    */'**')
      tcc_prefix=${tcc_target%/**}
      awk -v prefix="$tcc_prefix/" \
        'index($0, prefix) == 1 { count++ } END { print count + 0 }' \
        "$tcc_manifest"
      ;;
    *)
      awk -v prefix="$tcc_target:" \
        'index($0, prefix) == 1 { count++ } END { print count + 0 }' \
        "$tcc_manifest"
      ;;
  esac
}

validate_mutation_scope_manifest() {
  vmsm_manifest=$1
  validate_declared_mutation_scope
  validate_list_file "$vmsm_manifest"
  vmsm_bad=0
  vmsm_oldifs=$IFS
  set -f
  IFS='
'
  for vmsm_target in $MUTATION_TARGETS; do
    vmsm_count=$(target_candidate_count "$vmsm_manifest" "$vmsm_target")
    if [ "$vmsm_count" -lt 1 ]; then
      printf 'error: mutation target matches no candidates: %s\n' "$vmsm_target" >&2
      vmsm_bad=1
    else
      printf 'mutation:   %-36s %s candidates\n' "$vmsm_target" "$vmsm_count"
    fi
  done
  set +f
  IFS=$vmsm_oldifs

  while IFS='	' read -r vmsm_source vmsm_name; do
    if ! awk -v prefix="$vmsm_source:" -v site="$vmsm_name" '
        index($0, prefix) == 1 &&
          index($0, "replace " site " ->") { found = 1 }
        END { exit !found }
      ' "$vmsm_manifest"; then
      printf 'error: cargo-mutants omitted required firing site: %s::%s\n' \
        "$vmsm_source" "$vmsm_name" >&2
      vmsm_bad=1
    fi
  done <<EOF
$(print_optimizer_firing_sites)
EOF
  [ "$vmsm_bad" -eq 0 ] || return 1
  printf 'mutation: candidate manifest covers every declared target and optimizer firing site\n'
}

set_mode() {
  if [ "$MODE" != broad ]; then
    die "mutation modes cannot be combined"
  fi
  MODE=$1
}

append_shard_output() {
  if [ -z "$SHARD_OUTPUTS" ]; then
    SHARD_OUTPUTS=$1
  else
    SHARD_OUTPUTS="$SHARD_OUTPUTS
$1"
  fi
}

require_temp_root() {
  : "${TMPDIR:?deterministic mutation modes require TMPDIR}"
  [ -d "$TMPDIR" ] || die "TMPDIR is not a directory: $TMPDIR"
}

require_tool() {
  if ! sh "$REPO_ROOT/ci/cargo.sh" mutants --version >/dev/null 2>&1; then
    printf 'error: cargo-mutants is not installed.\n' >&2
    printf 'Install the pinned optional Cargo tools with:\n' >&2
    printf '  sh ci/impl-toolchain.sh install-report-tools\n' >&2
    exit 2
  fi
}

tool_version() {
  sh "$REPO_ROOT/ci/cargo.sh" mutants --version
}

require_clean_checkout() {
  rc_status=$(git -C "$REPO_ROOT" status --porcelain=v1 --untracked-files=all) ||
    die "cannot inspect the repository worktree"
  [ -z "$rc_status" ] ||
    die "deterministic mutation modes require a clean commit"
}

require_clean_commit() {
  rcc_commit=$1
  require_clean_checkout
  rcc_head=$(git -C "$REPO_ROOT" rev-parse HEAD) ||
    die "cannot resolve the repository commit"
  [ "$rcc_head" = "$rcc_commit" ] ||
    die "the repository commit changed during deterministic mutation work"
}

# Deterministic artifacts live outside the worktree. This keeps the clean-commit
# proof meaningful after cargo-mutants creates its output directory.
external_new_dir() {
  end_path=$1
  [ ! -e "$end_path" ] || die "output already exists: $end_path"
  end_parent=$(dirname -- "$end_path")
  [ -d "$end_parent" ] || die "output parent does not exist: $end_parent"
  end_parent=$(CDPATH='' cd -- "$end_parent" && pwd -P) ||
    die "cannot resolve output parent: $end_parent"
  end_base=$(basename -- "$end_path")
  if [ "$end_base" = . ] || [ "$end_base" = .. ]; then
    die "invalid output path: $end_path"
  fi
  NEW_DIR=$end_parent/$end_base
  case "$NEW_DIR" in
    "$REPO_ROOT"|"$REPO_ROOT"/*)
      die "deterministic mutation artifacts must be outside the worktree"
      ;;
  esac
}

validate_timeout() {
  case "$TIMEOUT" in
    ''|*[!0-9]*) die "timeout must be a non-negative integer" ;;
  esac
}

parse_shard() {
  ps_spec=$1
  case "$ps_spec" in
    */*) ;;
    *) die "shard must have the zero-based form K/N" ;;
  esac
  case "$ps_spec" in
    */*/*) die "shard must have the zero-based form K/N" ;;
  esac
  SHARD_INDEX=${ps_spec%/*}
  SHARD_COUNT=${ps_spec#*/}
  case "$SHARD_INDEX" in
    ''|*[!0-9]*) die "shard index must be a non-negative integer" ;;
    0|[1-9]*) ;;
    *) die "shard index is not canonical: $SHARD_INDEX" ;;
  esac
  case "$SHARD_COUNT" in
    ''|*[!0-9]*|0) die "shard count must be a positive integer" ;;
    [1-9]*) ;;
    *) die "shard count is not canonical: $SHARD_COUNT" ;;
  esac
  [ "$SHARD_INDEX" -lt "$SHARD_COUNT" ] ||
    die "shard index must be smaller than shard count"
}

validate_list_file() {
  vlf_path=$1
  [ -f "$vlf_path" ] || die "missing mutation manifest: $vlf_path"
  if ! awk '
      NF == 0 || index($0, "\t") || index($0, "\r") { exit 1 }
      END { if (NR == 0) exit 1 }
    ' "$vlf_path"; then
    die "mutation manifest is empty or contains an invalid line"
  fi
  vlf_duplicates=$(LC_ALL=C sort "$vlf_path" | uniq -d | sed -n '1p')
  [ -z "$vlf_duplicates" ] || die "mutation manifest contains duplicate rows"
  LIST_COUNT=$(awk 'END { print NR + 0 }' "$vlf_path")
}

validate_metadata() {
  vm_path=$1
  vm_kind=$2
  [ -f "$vm_path" ] || die "missing mutation metadata: $vm_path"
  if ! awk -F= -v kind="$vm_kind" '
      BEGIN {
        allowed["schema"] = 1
        allowed["commit"] = 1
        allowed["cargo_mutants_version"] = 1
        allowed["config_blob"] = 1
        allowed["source_blob"] = 1
        allowed["features"] = 1
        allowed["manifest_blob"] = 1
        allowed["manifest_json_blob"] = 1
        allowed["count"] = 1
        expected = 9
        if (kind == "shard") {
          allowed["shard"] = 1
          allowed["command_status"] = 1
          allowed["run_complete"] = 1
          allowed["outcomes_blob"] = 1
          expected = 13
        }
      }
      NF != 2 || !($1 in allowed) || $2 == "" || seen[$1]++ { bad = 1 }
      END {
        if (bad || NR != expected) exit 1
        for (key in allowed) if (seen[key] != 1) exit 1
      }
    ' "$vm_path"; then
    die "malformed $vm_kind metadata: $vm_path"
  fi
}

metadata_value() {
  mv_path=$1
  mv_key=$2
  awk -F= -v key="$mv_key" '$1 == key { print substr($0, length(key) + 2) }' \
    "$mv_path"
}

create_manifest_pair() {
  cmp_dir=$1
  (
    cd "$REPO_ROOT/kio-rs"
    sh "$REPO_ROOT/ci/cargo.sh" mutants --list --no-shuffle \
      --file "$EQUIV_SOURCE" >"$cmp_dir/manifest.txt"
    sh "$REPO_ROOT/ci/cargo.sh" mutants --list --json --no-shuffle \
      --file "$EQUIV_SOURCE" >"$cmp_dir/manifest.json"
  )
  [ -s "$cmp_dir/manifest.json" ] || die "cargo-mutants produced empty JSON"
  validate_list_file "$cmp_dir/manifest.txt"
}

write_common_metadata() {
  wcm_path=$1
  wcm_commit=$2
  wcm_version=$3
  wcm_config=$4
  wcm_source=$5
  wcm_manifest=$6
  wcm_manifest_json=$7
  wcm_count=$8
  {
    printf 'schema=1\n'
    printf 'commit=%s\n' "$wcm_commit"
    printf 'cargo_mutants_version=%s\n' "$wcm_version"
    printf 'config_blob=%s\n' "$wcm_config"
    printf 'source_blob=%s\n' "$wcm_source"
    printf 'features=default\n'
    printf 'manifest_blob=%s\n' "$wcm_manifest"
    printf 'manifest_json_blob=%s\n' "$wcm_manifest_json"
    printf 'count=%s\n' "$wcm_count"
  } >"$wcm_path"
}

validate_manifest_dir() {
  vmd_dir=$1
  [ -d "$vmd_dir" ] || die "manifest directory does not exist: $vmd_dir"
  validate_metadata "$vmd_dir/metadata" manifest
  validate_list_file "$vmd_dir/manifest.txt"
  [ -s "$vmd_dir/manifest.json" ] ||
    die "missing or empty machine-readable manifest: $vmd_dir/manifest.json"
  if ! awk 'index($0, "src/normalization.rs:") != 1 { exit 1 }' \
    "$vmd_dir/manifest.txt"; then
    die "evaluator manifest contains a mutant outside src/normalization.rs"
  fi

  MANIFEST_SCHEMA=$(metadata_value "$vmd_dir/metadata" schema)
  MANIFEST_COMMIT=$(metadata_value "$vmd_dir/metadata" commit)
  MANIFEST_VERSION=$(metadata_value "$vmd_dir/metadata" cargo_mutants_version)
  MANIFEST_CONFIG=$(metadata_value "$vmd_dir/metadata" config_blob)
  MANIFEST_SOURCE=$(metadata_value "$vmd_dir/metadata" source_blob)
  MANIFEST_FEATURES=$(metadata_value "$vmd_dir/metadata" features)
  MANIFEST_BLOB=$(metadata_value "$vmd_dir/metadata" manifest_blob)
  MANIFEST_JSON_BLOB=$(metadata_value "$vmd_dir/metadata" manifest_json_blob)
  MANIFEST_COUNT=$(metadata_value "$vmd_dir/metadata" count)

  [ "$MANIFEST_SCHEMA" = 1 ] || die "unsupported manifest schema: $MANIFEST_SCHEMA"
  [ "$MANIFEST_FEATURES" = default ] || die "unsupported feature set: $MANIFEST_FEATURES"
  case "$MANIFEST_COUNT" in
    ''|*[!0-9]*|0) die "manifest count must be positive" ;;
  esac
  [ "$MANIFEST_COUNT" -eq "$LIST_COUNT" ] ||
    die "manifest count does not match manifest.txt"
  vmd_blob=$(git hash-object "$vmd_dir/manifest.txt") ||
    die "cannot hash manifest.txt"
  [ "$MANIFEST_BLOB" = "$vmd_blob" ] ||
    die "manifest.txt does not match its recorded blob"
  vmd_json_blob=$(git hash-object "$vmd_dir/manifest.json") ||
    die "cannot hash manifest.json"
  [ "$MANIFEST_JSON_BLOB" = "$vmd_json_blob" ] ||
    die "manifest.json does not match its recorded blob"
}

authenticate_manifest() {
  am_dir=$1
  validate_manifest_dir "$am_dir"
  require_clean_checkout
  am_head=$(git -C "$REPO_ROOT" rev-parse HEAD)
  [ "$MANIFEST_COMMIT" = "$am_head" ] || die "manifest commit is stale"
  am_version=$(tool_version)
  [ "$MANIFEST_VERSION" = "$am_version" ] || die "cargo-mutants version drifted"
  am_config=$(git -C "$REPO_ROOT" rev-parse "HEAD:$EQUIV_CONFIG_REPO")
  am_source=$(git -C "$REPO_ROOT" rev-parse "HEAD:$EQUIV_SOURCE_REPO")
  [ "$MANIFEST_CONFIG" = "$am_config" ] || die "cargo-mutants config drifted"
  [ "$MANIFEST_SOURCE" = "$am_source" ] || die "normalization source drifted"

  am_refresh=$(mktemp -d "$TMPDIR/equiv-mutants-refresh.XXXXXX") ||
    die "cannot create manifest refresh directory"
  CLEANUP_DIR=$am_refresh
  create_manifest_pair "$am_refresh"
  cmp -s "$am_dir/manifest.txt" "$am_refresh/manifest.txt" ||
    die "cargo-mutants text inventory drifted"
  cmp -s "$am_dir/manifest.json" "$am_refresh/manifest.json" ||
    die "cargo-mutants JSON inventory drifted"
  require_clean_commit "$MANIFEST_COMMIT"
  rm -rf "$am_refresh"
  CLEANUP_DIR=
}

create_equiv_manifest() {
  require_temp_root
  require_tool
  require_clean_checkout
  external_new_dir "$MANIFEST_DIR"
  cem_target=$NEW_DIR
  cem_commit=$(git -C "$REPO_ROOT" rev-parse HEAD)
  cem_stage=$(mktemp -d "$TMPDIR/equiv-mutants-manifest.XXXXXX") ||
    die "cannot create manifest staging directory"
  CLEANUP_DIR=$cem_stage
  create_manifest_pair "$cem_stage"
  require_clean_commit "$cem_commit"

  cem_version=$(tool_version)
  cem_config=$(git -C "$REPO_ROOT" rev-parse "$cem_commit:$EQUIV_CONFIG_REPO")
  cem_source=$(git -C "$REPO_ROOT" rev-parse "$cem_commit:$EQUIV_SOURCE_REPO")
  cem_blob=$(git hash-object "$cem_stage/manifest.txt")
  cem_json_blob=$(git hash-object "$cem_stage/manifest.json")
  write_common_metadata "$cem_stage/metadata" "$cem_commit" "$cem_version" \
    "$cem_config" "$cem_source" "$cem_blob" "$cem_json_blob" "$LIST_COUNT"
  validate_manifest_dir "$cem_stage"
  require_clean_commit "$cem_commit"
  mv "$cem_stage" "$cem_target"
  CLEANUP_DIR=
  printf 'mutation: wrote %s evaluator mutants to %s\n' "$LIST_COUNT" "$cem_target"
}

expected_shard() {
  es_manifest=$1
  es_index=$2
  es_count=$3
  es_output=$4
  awk -v shard_index="$es_index" -v shard_count="$es_count" \
    '((NR - 1) % shard_count) == shard_index { print }' \
    "$es_manifest" >"$es_output"
}

write_shard_metadata() {
  wsm_path=$1
  wsm_status=$2
  wsm_complete=$3
  wsm_outcomes=$4
  write_common_metadata "$wsm_path" "$MANIFEST_COMMIT" "$MANIFEST_VERSION" \
    "$MANIFEST_CONFIG" "$MANIFEST_SOURCE" "$MANIFEST_BLOB" \
    "$MANIFEST_JSON_BLOB" "$MANIFEST_COUNT"
  {
    printf 'shard=%s\n' "$SHARD_SPEC"
    printf 'command_status=%s\n' "$wsm_status"
    printf 'run_complete=%s\n' "$wsm_complete"
    printf 'outcomes_blob=%s\n' "$wsm_outcomes"
  } >>"$wsm_path"
}

outcomes_blob() {
  ob_dir=$1
  for ob_name in caught missed timeout unviable; do
    [ -f "$ob_dir/$ob_name.txt" ] || return 1
  done
  {
    for ob_name in caught missed timeout unviable; do
      printf '[%s]\n' "$ob_name"
      cat "$ob_dir/$ob_name.txt"
    done
  } | git hash-object --stdin
}

run_equiv_shard() {
  require_temp_root
  require_tool
  validate_timeout
  parse_shard "$SHARD_SPEC"
  authenticate_manifest "$MANIFEST_DIR"
  [ "$SHARD_COUNT" -le "$MANIFEST_COUNT" ] ||
    die "shard count exceeds the mutation count"
  external_new_dir "$OUTPUT_DIR"
  res_output=$NEW_DIR

  res_scratch=$(mktemp -d "$TMPDIR/equiv-mutants-shard.XXXXXX") ||
    die "cannot create shard staging directory"
  CLEANUP_DIR=$res_scratch
  expected_shard "$MANIFEST_DIR/manifest.txt" "$SHARD_INDEX" "$SHARD_COUNT" \
    "$res_scratch/expected.txt"
  (
    cd "$REPO_ROOT/kio-rs"
    sh "$REPO_ROOT/ci/cargo.sh" mutants --list --no-shuffle \
      --file "$EQUIV_SOURCE" --shard "$SHARD_SPEC" >"$res_scratch/tool.txt"
  )
  cmp -s "$res_scratch/expected.txt" "$res_scratch/tool.txt" ||
    die "cargo-mutants shard selection disagrees with the modulo partition"

  # `--in-place` is load-bearing. cargo-mutants' default copies only the
  # standalone kio-rs workspace, while typer tests include canonical fixtures
  # from the sibling test-data tree. Deterministic campaigns use small shards
  # rather than an outer wall-clock killer: cargo-mutants owns its portable
  # per-test timeout and process-tree cleanup, including source restoration.
  res_status=0
  (
    cd "$REPO_ROOT/kio-rs"
    sh "$REPO_ROOT/ci/cargo.sh" mutants --in-place --no-shuffle --no-times \
      --file "$EQUIV_SOURCE" --shard "$SHARD_SPEC" --output "$res_output"
  ) || res_status=$?

  case "$res_status" in
    0|2|3) res_complete=1 ;;
    *) res_complete=0 ;;
  esac
  mkdir -p "$res_output/mutants.out"
  cp "$res_scratch/expected.txt" "$res_output/mutants.out/selection.txt"
  : >"$res_output/mutants.out/triage.tsv"
  res_outcomes_blob=unavailable
  if res_digest=$(outcomes_blob "$res_output/mutants.out"); then
    res_outcomes_blob=$res_digest
  fi
  write_shard_metadata "$res_output/mutants.out/equiv-shard.meta" \
    "$res_status" 0 "$res_outcomes_blob"

  require_clean_commit "$MANIFEST_COMMIT"
  if [ "$res_complete" -eq 1 ]; then
    res_outcomes_blob=$(outcomes_blob "$res_output/mutants.out") ||
      die "completed cargo-mutants output is missing an outcome file"
    write_shard_metadata "$res_output/mutants.out/equiv-shard.meta" \
      "$res_status" 1 "$res_outcomes_blob"
  fi
  rm -rf "$res_scratch"
  CLEANUP_DIR=
  case "$res_status" in
    0|2|3)
      printf 'mutation: deterministic shard %s completed in %s\n' \
        "$SHARD_SPEC" "$res_output"
      ;;
    *)
      printf 'mutation: cargo-mutants failed with exit %d; output preserved in %s\n' \
        "$res_status" "$res_output" >&2
      exit "$res_status"
      ;;
  esac
}

compare_common_metadata() {
  ccm_path=$1
  for ccm_key in schema commit cargo_mutants_version config_blob source_blob \
    features manifest_blob manifest_json_blob count; do
    ccm_want=$(metadata_value "$MANIFEST_DIR/metadata" "$ccm_key")
    ccm_got=$(metadata_value "$ccm_path" "$ccm_key")
    [ "$ccm_want" = "$ccm_got" ] ||
      die "shard metadata disagrees on $ccm_key: $ccm_path"
  done
}

validate_outcome_evidence() {
  voe_dir=$1
  voe_selection=$2
  voe_status=$3
  for voe_name in caught missed timeout unviable; do
    [ -f "$voe_dir/$voe_name.txt" ] ||
      die "missing shard outcome: $voe_dir/$voe_name.txt"
    if ! awk 'NF == 0 || index($0, "\t") || index($0, "\r") { exit 1 }' \
      "$voe_dir/$voe_name.txt"; then
      die "invalid row in shard outcome: $voe_dir/$voe_name.txt"
    fi
  done
  voe_digest=$(outcomes_blob "$voe_dir") ||
    die "cannot hash shard outcome evidence: $voe_dir"
  voe_recorded=$(metadata_value "$voe_dir/equiv-shard.meta" outcomes_blob)
  [ "$voe_digest" = "$voe_recorded" ] ||
    die "shard outcome categories changed after the run: $voe_dir"
  case "$voe_status" in
    0)
      if [ -s "$voe_dir/missed.txt" ] || [ -s "$voe_dir/timeout.txt" ]; then
        die "command status 0 cannot contain missed or timed-out mutants"
      fi
      ;;
    2)
      [ -s "$voe_dir/missed.txt" ] ||
        die "command status 2 requires a missed mutant"
      [ ! -s "$voe_dir/timeout.txt" ] ||
        die "a timeout takes precedence over command status 2"
      ;;
    3)
      [ -s "$voe_dir/timeout.txt" ] ||
        die "command status 3 requires a timed-out mutant"
      ;;
  esac
  cat "$voe_dir/caught.txt" "$voe_dir/missed.txt" \
    "$voe_dir/timeout.txt" "$voe_dir/unviable.txt" >"$CLEANUP_DIR/outcomes"
  LC_ALL=C sort "$CLEANUP_DIR/outcomes" >"$CLEANUP_DIR/outcomes.sorted"
  LC_ALL=C sort "$voe_selection" >"$CLEANUP_DIR/selection.sorted"
  cmp -s "$CLEANUP_DIR/outcomes.sorted" "$CLEANUP_DIR/selection.sorted" ||
    die "shard outcomes omit, duplicate, or add a selected mutant"

  [ -f "$voe_dir/triage.tsv" ] || die "missing shard triage: $voe_dir/triage.tsv"
  if ! awk -F '\t' '
      NF != 3 { exit 1 }
      $1 != "equivalent" && $1 != "test-gap" && $1 != "unresolved" { exit 1 }
      $2 == "" || $3 !~ /[^[:space:]]/ { exit 1 }
    ' "$voe_dir/triage.tsv"; then
    die "malformed shard triage: $voe_dir/triage.tsv"
  fi
  cat "$voe_dir/missed.txt" "$voe_dir/timeout.txt" \
    "$voe_dir/unviable.txt" >"$CLEANUP_DIR/noncaught"
  cut -f 2 "$voe_dir/triage.tsv" >"$CLEANUP_DIR/triaged"
  LC_ALL=C sort "$CLEANUP_DIR/noncaught" >"$CLEANUP_DIR/noncaught.sorted"
  LC_ALL=C sort "$CLEANUP_DIR/triaged" >"$CLEANUP_DIR/triaged.sorted"
  cmp -s "$CLEANUP_DIR/noncaught.sorted" "$CLEANUP_DIR/triaged.sorted" ||
    die "triage must classify every non-caught mutant exactly once"
  if awk -F '\t' '$1 == "test-gap" || $1 == "unresolved" { found = 1 }
      END { exit !found }' "$voe_dir/triage.tsv"; then
    : >"$CLEANUP_DIR/open-triage"
  fi
}

validate_equiv_shards() {
  require_temp_root
  validate_manifest_dir "$MANIFEST_DIR"
  [ -n "$SHARD_OUTPUTS" ] || die "--equiv-validate needs at least one --shard-output"
  ves_scratch=$(mktemp -d "$TMPDIR/equiv-mutants-validate.XXXXXX") ||
    die "cannot create validation directory"
  CLEANUP_DIR=$ves_scratch
  : >"$ves_scratch/shards"
  : >"$ves_scratch/all-selections"
  ves_denominator=

  ves_oldifs=$IFS
  set -f
  IFS='
'
  for ves_output in $SHARD_OUTPUTS; do
    ves_dir=$ves_output/mutants.out
    [ -d "$ves_dir" ] || die "missing mutants.out directory: $ves_output"
    validate_metadata "$ves_dir/equiv-shard.meta" shard
    compare_common_metadata "$ves_dir/equiv-shard.meta"
    ves_complete=$(metadata_value "$ves_dir/equiv-shard.meta" run_complete)
    [ "$ves_complete" = 1 ] || die "shard run is incomplete: $ves_output"
    ves_status=$(metadata_value "$ves_dir/equiv-shard.meta" command_status)
    case "$ves_status" in 0|2|3) ;; *) die "invalid completed command status: $ves_status" ;; esac
    ves_shard=$(metadata_value "$ves_dir/equiv-shard.meta" shard)
    parse_shard "$ves_shard"
    if [ -z "$ves_denominator" ]; then
      ves_denominator=$SHARD_COUNT
    else
      [ "$ves_denominator" = "$SHARD_COUNT" ] ||
        die "shard outputs use different shard counts"
    fi
    [ "$SHARD_COUNT" -le "$MANIFEST_COUNT" ] ||
      die "shard count exceeds the mutation count"
    printf '%s\n' "$ves_shard" >>"$ves_scratch/shards"
    [ -f "$ves_dir/selection.txt" ] || die "missing shard selection: $ves_dir"
    expected_shard "$MANIFEST_DIR/manifest.txt" "$SHARD_INDEX" "$SHARD_COUNT" \
      "$ves_scratch/expected"
    cmp -s "$ves_scratch/expected" "$ves_dir/selection.txt" ||
      die "stored shard selection is not the exact modulo partition: $ves_shard"
    validate_outcome_evidence "$ves_dir" "$ves_dir/selection.txt" "$ves_status"
    cat "$ves_dir/selection.txt" >>"$ves_scratch/all-selections"
  done
  IFS=$ves_oldifs
  set +f

  ves_duplicates=$(LC_ALL=C sort "$ves_scratch/shards" | uniq -d)
  [ -z "$ves_duplicates" ] || die "duplicate shard output: $ves_duplicates"
  ves_shard_rows=$(awk 'END { print NR + 0 }' "$ves_scratch/shards")
  [ "$ves_shard_rows" -eq "$ves_denominator" ] ||
    die "expected $ves_denominator shard outputs, got $ves_shard_rows"
  ves_index=0
  while [ "$ves_index" -lt "$ves_denominator" ]; do
    grep -Fqx "$ves_index/$ves_denominator" "$ves_scratch/shards" ||
      die "missing shard output: $ves_index/$ves_denominator"
    ves_index=$((ves_index + 1))
  done
  LC_ALL=C sort "$ves_scratch/all-selections" >"$ves_scratch/all-selections.sorted"
  LC_ALL=C sort "$MANIFEST_DIR/manifest.txt" >"$ves_scratch/manifest.sorted"
  cmp -s "$ves_scratch/all-selections.sorted" "$ves_scratch/manifest.sorted" ||
    die "shard union is not the complete mutation manifest"

  ves_open=0
  [ ! -e "$ves_scratch/open-triage" ] || ves_open=1
  rm -rf "$ves_scratch"
  CLEANUP_DIR=
  if [ "$ves_open" -eq 1 ]; then
    printf 'mutation: complete shard evidence has unresolved or test-gap triage\n' >&2
    exit 3
  fi
  printf 'mutation: complete shard evidence validated; all non-caught mutants are equivalent\n'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --print-targets)
      printf '%s\n' "$MUTATION_TARGETS"
      exit 0
      ;;
    --print-production-sources)
      print_mutation_production_sources
      exit 0
      ;;
    --print-firing-sites)
      print_optimizer_firing_sites
      exit 0
      ;;
    --scope-self-test)
      validate_declared_mutation_scope
      exit 0
      ;;
    --validate-scope-manifest=*)
      validate_mutation_scope_manifest "${1#--validate-scope-manifest=}"
      exit 0
      ;;
    --validate-scope-manifest)
      shift
      [ $# -gt 0 ] || die "--validate-scope-manifest requires a file"
      validate_mutation_scope_manifest "$1"
      exit 0
      ;;
    --timeout=*) TIMEOUT=${1#--timeout=}; TIMEOUT_SET=1 ;;
    --timeout)
      shift
      [ $# -gt 0 ] || die "--timeout requires a value"
      TIMEOUT=$1
      TIMEOUT_SET=1
      ;;
    --equiv-manifest=*)
      set_mode manifest
      MANIFEST_DIR=${1#--equiv-manifest=}
      ;;
    --equiv-shard=*)
      set_mode shard
      SHARD_SPEC=${1#--equiv-shard=}
      ;;
    --equiv-validate) set_mode validate ;;
    --manifest=*) MANIFEST_DIR=${1#--manifest=} ;;
    --manifest)
      shift
      [ $# -gt 0 ] || die "--manifest requires a directory"
      MANIFEST_DIR=$1
      ;;
    --output=*) OUTPUT_DIR=${1#--output=} ;;
    --output)
      shift
      [ $# -gt 0 ] || die "--output requires a directory"
      OUTPUT_DIR=$1
      ;;
    --shard-output=*) append_shard_output "${1#--shard-output=}" ;;
    --shard-output)
      shift
      [ $# -gt 0 ] || die "--shard-output requires a directory"
      append_shard_output "$1"
      ;;
    -h|--help)
      cat <<EOF
Usage:
  sh $0 [--timeout=<seconds>]
  sh $0 --print-targets|--print-production-sources|--print-firing-sites
  sh $0 --scope-self-test
  sh $0 --validate-scope-manifest=<cargo-mutants-list>
  sh $0 --equiv-manifest=<dir>
  sh $0 --equiv-shard=K/N --manifest=<dir> --output=<dir>
  sh $0 --equiv-validate --manifest=<dir> --shard-output=<dir> [...]

The default is the bounded, shuffled semantic-core report. The evaluator-only
forms create an authenticated clean-commit manifest, run a deterministic
zero-based modulo shard, or validate a complete set of shard artifacts offline.
Every missed, timed-out, or unviable mutant needs an equivalent, test-gap, or
unresolved row with a rationale in mutants.out/triage.tsv. Validation exits 3
while any test-gap or unresolved classification remains.

--timeout applies only to the broad report, defaults to 900 seconds, and uses
0 to disable that wall-clock cap. Bound deterministic work with smaller shards.
--print-targets prints the broad report's target globs and exits.
--print-production-sources and --print-firing-sites print the scope contract.
--scope-self-test proves every claimed production source and optimizer catalog
arm is covered by the declared targets. --validate-scope-manifest additionally
proves that a real cargo-mutants candidate list reaches every target and arm.
EOF
      exit 0
      ;;
    *) die "unknown argument: $1" ;;
  esac
  shift
done

case "$MODE" in
  manifest)
    [ -n "$MANIFEST_DIR" ] || die "--equiv-manifest requires a directory"
    [ -z "$OUTPUT_DIR$SHARD_OUTPUTS" ] || die "unexpected output argument in manifest mode"
    [ "$TIMEOUT_SET" -eq 0 ] || die "--timeout does not apply to manifest mode"
    create_equiv_manifest
    exit 0
    ;;
  shard)
    [ -n "$MANIFEST_DIR" ] || die "--equiv-shard requires --manifest"
    [ -n "$OUTPUT_DIR" ] || die "--equiv-shard requires --output"
    [ -z "$SHARD_OUTPUTS" ] || die "--shard-output applies only to validation"
    [ "$TIMEOUT_SET" -eq 0 ] ||
      die "--timeout applies only to the broad report; bound deterministic work with smaller shards"
    run_equiv_shard
    exit 0
    ;;
  validate)
    [ -n "$MANIFEST_DIR" ] || die "--equiv-validate requires --manifest"
    [ -z "$OUTPUT_DIR$SHARD_SPEC" ] || die "unexpected shard-run argument in validation mode"
    [ "$TIMEOUT_SET" -eq 0 ] || die "--timeout does not apply to validation"
    validate_equiv_shards
    exit 0
    ;;
  broad) ;;
  *) die "internal mutation mode error: $MODE" ;;
esac

[ -z "$MANIFEST_DIR$OUTPUT_DIR$SHARD_SPEC$SHARD_OUTPUTS" ] ||
  die "deterministic arguments require an --equiv-* mode"
validate_timeout
require_tool
cd "$REPO_ROOT/kio-rs"

# Rebuild the positional parameters as cargo-mutants `--file` arguments.
# `set -f` keeps the `**` globs literal for cargo-mutants to expand.
_oldifs=$IFS
set -f
IFS='
'
set --
for _t in $MUTATION_TARGETS; do
  set -- "$@" --file "$_t"
done
set +f
IFS=$_oldifs

exit_code=0
if [ "$TIMEOUT" -gt 0 ]; then
  timeout "$TIMEOUT" sh "$REPO_ROOT/ci/cargo.sh" mutants --in-place "$@" --shuffle || exit_code=$?
else
  sh "$REPO_ROOT/ci/cargo.sh" mutants --in-place "$@" --shuffle || exit_code=$?
fi

case "$exit_code" in
  0) printf 'mutation: all mutants caught within budget\n' ;;
  2) printf 'mutation: at least one mutant survived (see report above)\n' ;;
  3) printf 'mutation: at least one mutant timed out (see report above)\n' ;;
  124) printf 'mutation: wall-clock budget reached (%ds); partial coverage\n' "$TIMEOUT" ;;
  *)
    printf 'mutation: cargo-mutants failed with exit %d\n' "$exit_code" >&2
    exit "$exit_code"
    ;;
esac
