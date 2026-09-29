#!/bin/sh
#
# Exercise the offline mutation-shard validator with synthetic cargo-mutants
# artifacts. The fixture needs no cargo-mutants installation or network access.
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
MUTATION_SH=${MUTATION_SH:-$REPO_ROOT/reports/mutation.sh}

if [ -z "${TMPDIR:-}" ]; then
  TMPDIR=$REPO_ROOT/target
  mkdir -p "$TMPDIR"
fi
export TMPDIR
if [ ! -d "$TMPDIR" ]; then
  printf 'mutation-shard-selftest: TMPDIR is not a directory: %s\n' "$TMPDIR" >&2
  exit 2
fi
if [ ! -f "$MUTATION_SH" ]; then
  printf 'mutation-shard-selftest: cannot find %s\n' "$MUTATION_SH" >&2
  exit 2
fi

scratch=$(mktemp -d "$TMPDIR/mutation-shard-selftest.XXXXXX") || {
  printf 'mutation-shard-selftest: cannot create scratch directory\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' 0
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

mutant_1='src/normalization.rs:10: replace + with -'
mutant_2='src/normalization.rs:20: replace == with !='
mutant_3='src/normalization.rs:30: delete match arm'
mutant_4='src/normalization.rs:40: replace true with false'
foreign_mutant='src/normalization.rs:99: replace true with false'

write_metadata() {
  wm_path=$1
  wm_shard=${2:-}
  wm_status=${3:-}
  wm_outcomes=${4:-}
  {
    printf '%s\n' \
      'schema=1' \
      'commit=0123456789abcdef0123456789abcdef01234567' \
      'cargo_mutants_version=25.3.1' \
      'config_blob=1111111111111111111111111111111111111111111111111111111111111111' \
      'source_blob=2222222222222222222222222222222222222222222222222222222222222222' \
      'features=default' \
      "manifest_blob=$manifest_blob" \
      "manifest_json_blob=$manifest_json_blob" \
      'count=4'
    if [ -n "$wm_shard" ]; then
      printf '%s\n' \
        "shard=$wm_shard" \
        "outcomes_blob=$wm_outcomes" \
        "command_status=$wm_status" \
        'run_complete=1'
    fi
  } >"$wm_path"
}

hash_outcomes() {
  ho_dir=$1
  {
    for ho_name in caught missed timeout unviable; do
      printf '[%s]\n' "$ho_name"
      cat "$ho_dir/$ho_name.txt"
    done
  } | git hash-object --stdin
}

set_metadata_value() {
  sm_file=$1
  sm_key=$2
  sm_value=$3
  awk -F '=' -v key="$sm_key" -v value="$sm_value" '
    $1 == key { print key "=" value; found = 1; next }
    { print }
    END { if (!found) exit 1 }
  ' "$sm_file" >"$sm_file.new"
  mv "$sm_file.new" "$sm_file"
}

refresh_outcomes_blob() {
  ro_dir=$1
  ro_blob=$(hash_outcomes "$ro_dir") || {
    printf 'mutation-shard-selftest: cannot hash synthetic outcomes\n' >&2
    exit 2
  }
  set_metadata_value "$ro_dir/equiv-shard.meta" outcomes_blob "$ro_blob"
}

base=$scratch/base
manifest=$base/manifest
shard_0=$base/shard-0/mutants.out
shard_1=$base/shard-1/mutants.out
mkdir -p "$manifest" "$shard_0" "$shard_1"
printf '%s\n' "$mutant_1" "$mutant_2" "$mutant_3" "$mutant_4" \
  >"$manifest/manifest.txt"
manifest_blob=$(git hash-object "$manifest/manifest.txt") || {
  printf 'mutation-shard-selftest: cannot hash synthetic manifest\n' >&2
  exit 2
}
printf '{"mutants":["%s","%s","%s","%s"]}\n' \
  "$mutant_1" "$mutant_2" "$mutant_3" "$mutant_4" \
  >"$manifest/manifest.json"
manifest_json_blob=$(git hash-object "$manifest/manifest.json") || {
  printf 'mutation-shard-selftest: cannot hash synthetic JSON manifest\n' >&2
  exit 2
}
write_metadata "$manifest/metadata"

printf '%s\n' "$mutant_1" "$mutant_3" >"$shard_0/selection.txt"
printf '%s\n' "$mutant_1" >"$shard_0/caught.txt"
printf '%s\n' "$mutant_3" >"$shard_0/missed.txt"
: >"$shard_0/timeout.txt"
: >"$shard_0/unviable.txt"
printf 'equivalent\t%s\tboth sides normalize to the same closed term\n' \
  "$mutant_3" >"$shard_0/triage.tsv"
shard_0_outcomes=$(hash_outcomes "$shard_0") || {
  printf 'mutation-shard-selftest: cannot hash shard 0 outcomes\n' >&2
  exit 2
}
write_metadata "$shard_0/equiv-shard.meta" '0/2' 2 "$shard_0_outcomes"

printf '%s\n' "$mutant_2" "$mutant_4" >"$shard_1/selection.txt"
: >"$shard_1/caught.txt"
: >"$shard_1/missed.txt"
printf '%s\n' "$mutant_2" >"$shard_1/timeout.txt"
printf '%s\n' "$mutant_4" >"$shard_1/unviable.txt"
{
  printf 'equivalent\t%s\tthe timeout is an equivalent evaluator loop bound\n' \
    "$mutant_2"
  printf 'equivalent\t%s\tthe replacement is unreachable for validated Prime\n' \
    "$mutant_4"
} >"$shard_1/triage.tsv"
shard_1_outcomes=$(hash_outcomes "$shard_1") || {
  printf 'mutation-shard-selftest: cannot hash shard 1 outcomes\n' >&2
  exit 2
}
write_metadata "$shard_1/equiv-shard.meta" '1/2' 3 "$shard_1_outcomes"

run_validator() {
  rv_fixture=$1
  rv_label=$2
  validator_status=0
  sh "$MUTATION_SH" --equiv-validate \
    --manifest "$rv_fixture/manifest" \
    --shard-output "$rv_fixture/shard-0" \
    --shard-output "$rv_fixture/shard-1" \
    >"$scratch/$rv_label.log" 2>&1 || validator_status=$?
}

fail_status() {
  fs_label=$1
  fs_wanted=$2
  cat "$scratch/$fs_label.log" >&2
  printf 'mutation-shard-selftest: %s: wanted exit %s, got %s\n' \
    "$fs_label" "$fs_wanted" "$validator_status" >&2
  exit 1
}

assert_status() {
  as_fixture=$1
  as_wanted=$2
  as_label=$3
  run_validator "$as_fixture" "$as_label"
  if [ "$validator_status" -ne "$as_wanted" ]; then
    fail_status "$as_label" "$as_wanted"
  fi
}

assert_rejected() {
  ar_fixture=$1
  ar_label=$2
  run_validator "$ar_fixture" "$ar_label"
  if [ "$validator_status" -eq 0 ]; then
    fail_status "$ar_label" nonzero
  fi
}

fresh_fixture() {
  ff_path=$1
  ff_label=$2
  cp -R "$base" "$ff_path"
  # The control run beside every mutation makes its later verdict causal,
  # rather than relying only on one shared success assertion.
  assert_status "$ff_path" 0 "$ff_label-control"
}

assert_status "$base" 0 success

omission=$scratch/omission
fresh_fixture "$omission" omission
sed -n '1p' "$omission/shard-1/mutants.out/selection.txt" \
  >"$omission/shard-1/mutants.out/selection.new"
mv "$omission/shard-1/mutants.out/selection.new" \
  "$omission/shard-1/mutants.out/selection.txt"
: >"$omission/shard-1/mutants.out/unviable.txt"
sed -n '1p' "$omission/shard-1/mutants.out/triage.tsv" \
  >"$omission/shard-1/mutants.out/triage.new"
mv "$omission/shard-1/mutants.out/triage.new" \
  "$omission/shard-1/mutants.out/triage.tsv"
refresh_outcomes_blob "$omission/shard-1/mutants.out"
if grep -Fq "$mutant_4" "$omission/shard-1/mutants.out/selection.txt" \
  "$omission/shard-1/mutants.out/unviable.txt" \
  "$omission/shard-1/mutants.out/triage.tsv"; then
  printf 'mutation-shard-selftest: omission mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$omission" omission

duplicate=$scratch/duplicate
fresh_fixture "$duplicate" duplicate
printf '%s\n' "$mutant_1" >>"$duplicate/shard-1/mutants.out/selection.txt"
printf '%s\n' "$mutant_1" >>"$duplicate/shard-1/mutants.out/caught.txt"
refresh_outcomes_blob "$duplicate/shard-1/mutants.out"
if ! grep -Fqx "$mutant_1" "$duplicate/shard-0/mutants.out/selection.txt" ||
   ! grep -Fqx "$mutant_1" "$duplicate/shard-1/mutants.out/selection.txt"; then
  printf 'mutation-shard-selftest: duplicate mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$duplicate" duplicate

foreign=$scratch/foreign
fresh_fixture "$foreign" foreign
printf '%s\n' "$foreign_mutant" >>"$foreign/shard-0/mutants.out/selection.txt"
printf '%s\n' "$foreign_mutant" >>"$foreign/shard-0/mutants.out/caught.txt"
refresh_outcomes_blob "$foreign/shard-0/mutants.out"
if ! grep -Fqx "$foreign_mutant" "$foreign/shard-0/mutants.out/selection.txt"; then
  printf 'mutation-shard-selftest: foreign-row mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$foreign" foreign-row

metadata_drift=$scratch/metadata-drift
fresh_fixture "$metadata_drift" metadata-drift
awk '/^source_blob=/ {
       print "source_blob=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
       next
     }
     { print }' "$metadata_drift/shard-1/mutants.out/equiv-shard.meta" \
  >"$metadata_drift/shard-1/mutants.out/equiv-shard.meta.new"
mv "$metadata_drift/shard-1/mutants.out/equiv-shard.meta.new" \
  "$metadata_drift/shard-1/mutants.out/equiv-shard.meta"
if ! grep -qx 'source_blob=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' \
  "$metadata_drift/shard-1/mutants.out/equiv-shard.meta"; then
  printf 'mutation-shard-selftest: metadata mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$metadata_drift" metadata-drift

json_drift=$scratch/json-drift
fresh_fixture "$json_drift" json-drift
printf ' \n' >>"$json_drift/manifest/manifest.json"
json_drift_blob=$(git hash-object "$json_drift/manifest/manifest.json") || {
  printf 'mutation-shard-selftest: cannot hash drifted JSON manifest\n' >&2
  exit 2
}
if [ "$json_drift_blob" = "$manifest_json_blob" ]; then
  printf 'mutation-shard-selftest: JSON content mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$json_drift" json-drift

category_swap=$scratch/category-swap
fresh_fixture "$category_swap" category-swap
printf '%s\n' "$mutant_3" >>"$category_swap/shard-0/mutants.out/caught.txt"
: >"$category_swap/shard-0/mutants.out/missed.txt"
: >"$category_swap/shard-0/mutants.out/triage.tsv"
set_metadata_value \
  "$category_swap/shard-0/mutants.out/equiv-shard.meta" command_status 0
category_swap_blob=$(hash_outcomes "$category_swap/shard-0/mutants.out") || {
  printf 'mutation-shard-selftest: cannot hash category-swapped outcomes\n' >&2
  exit 2
}
recorded_outcomes_blob=$(sed -n 's/^outcomes_blob=//p' \
  "$category_swap/shard-0/mutants.out/equiv-shard.meta")
if [ "$category_swap_blob" = "$recorded_outcomes_blob" ] ||
   ! grep -Fqx "$mutant_3" "$category_swap/shard-0/mutants.out/caught.txt" ||
   [ -s "$category_swap/shard-0/mutants.out/missed.txt" ] ||
   [ -s "$category_swap/shard-0/mutants.out/triage.tsv" ]; then
  printf 'mutation-shard-selftest: category-swap mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$category_swap" category-swap

status_mismatch=$scratch/status-mismatch
fresh_fixture "$status_mismatch" status-mismatch
set_metadata_value \
  "$status_mismatch/shard-0/mutants.out/equiv-shard.meta" command_status 0
status_mismatch_blob=$(hash_outcomes "$status_mismatch/shard-0/mutants.out") || {
  printf 'mutation-shard-selftest: cannot hash status-mismatch outcomes\n' >&2
  exit 2
}
status_mismatch_recorded=$(sed -n 's/^outcomes_blob=//p' \
  "$status_mismatch/shard-0/mutants.out/equiv-shard.meta")
if ! grep -qx 'command_status=0' \
  "$status_mismatch/shard-0/mutants.out/equiv-shard.meta" ||
   ! grep -Fqx "$mutant_3" \
     "$status_mismatch/shard-0/mutants.out/missed.txt" ||
   [ "$status_mismatch_blob" != "$status_mismatch_recorded" ]; then
  printf 'mutation-shard-selftest: status-mismatch mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$status_mismatch" status-mismatch

incomplete_run=$scratch/incomplete-run
fresh_fixture "$incomplete_run" incomplete-run
awk '/^run_complete=/ { print "run_complete=0"; next } { print }' \
  "$incomplete_run/shard-0/mutants.out/equiv-shard.meta" \
  >"$incomplete_run/shard-0/mutants.out/equiv-shard.meta.new"
mv "$incomplete_run/shard-0/mutants.out/equiv-shard.meta.new" \
  "$incomplete_run/shard-0/mutants.out/equiv-shard.meta"
if ! grep -qx 'run_complete=0' \
  "$incomplete_run/shard-0/mutants.out/equiv-shard.meta"; then
  printf 'mutation-shard-selftest: incomplete-run mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$incomplete_run" incomplete-run

missing_triage=$scratch/missing-triage
fresh_fixture "$missing_triage" missing-triage
rm "$missing_triage/shard-0/mutants.out/triage.tsv"
if [ -e "$missing_triage/shard-0/mutants.out/triage.tsv" ]; then
  printf 'mutation-shard-selftest: missing-triage mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$missing_triage" missing-triage

missing_manifest_json=$scratch/missing-manifest-json
fresh_fixture "$missing_manifest_json" missing-manifest-json
rm "$missing_manifest_json/manifest/manifest.json"
if [ -e "$missing_manifest_json/manifest/manifest.json" ]; then
  printf 'mutation-shard-selftest: missing-manifest-json mutation did not take effect\n' >&2
  exit 1
fi
assert_rejected "$missing_manifest_json" missing-manifest-json

test_gap=$scratch/test-gap
fresh_fixture "$test_gap" test-gap
awk -F '\t' 'BEGIN { OFS="\t" } { $1="test-gap"; print }' \
  "$test_gap/shard-0/mutants.out/triage.tsv" \
  >"$test_gap/shard-0/mutants.out/triage.new"
mv "$test_gap/shard-0/mutants.out/triage.new" \
  "$test_gap/shard-0/mutants.out/triage.tsv"
if ! awk -F '\t' '$1 == "test-gap" { found = 1 } END { exit !found }' \
  "$test_gap/shard-0/mutants.out/triage.tsv"; then
  printf 'mutation-shard-selftest: test-gap mutation did not take effect\n' >&2
  exit 1
fi
assert_status "$test_gap" 3 test-gap

unresolved=$scratch/unresolved
fresh_fixture "$unresolved" unresolved
awk -F '\t' 'BEGIN { OFS="\t" } { $1="unresolved"; print }' \
  "$unresolved/shard-1/mutants.out/triage.tsv" \
  >"$unresolved/shard-1/mutants.out/triage.new"
mv "$unresolved/shard-1/mutants.out/triage.new" \
  "$unresolved/shard-1/mutants.out/triage.tsv"
if ! awk -F '\t' '$1 == "unresolved" { found = 1 } END { exit !found }' \
  "$unresolved/shard-1/mutants.out/triage.tsv"; then
  printf 'mutation-shard-selftest: unresolved mutation did not take effect\n' >&2
  exit 1
fi
assert_status "$unresolved" 3 unresolved

printf 'mutation-shard-selftest: ok\n'
