#!/bin/sh
#
# Exercise field, mirror, rollout, default, rename, exclusion, and transition
# history.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
LINT="$REPO_ROOT/ci/checks/repo-lint/backend-api-stability.sh"

tmp_base=${KIO_TMP_DIR:-${TMPDIR:-/tmp}}
mkdir -p "$tmp_base"
scratch=$(mktemp -d "$tmp_base/backend-api-stability-selftest.XXXXXX") || {
  printf 'backend-api-stability-selftest: cannot make scratch dir\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

selftest_failures=0

write_backend() {
  repo=$1
  id=$2
  value=$3
  printf "# %s backend\n\n**Host API stability:** \`%s\`\n\nBackend contract.\n" \
    "$id" "$value" >"$repo/specs/backends/$id.md"
  printf "# Hosting %s\n\n**Host API stability:** \`%s\`\n\nHost guide.\n" \
    "$id" "$value" >"$repo/docs/hosts/$id.md"
}

write_backend_without_field() {
  repo=$1
  id=$2
  printf '# %s backend\n\nBackend contract.\n' \
    "$id" >"$repo/specs/backends/$id.md"
  printf '# Hosting %s\n\nHost guide.\n' \
    "$id" >"$repo/docs/hosts/$id.md"
}

set_status() {
  file=$1
  value=$2
  awk -v value="$value" '
    /^\*\*Host API stability:\*\*/ {
      print "**Host API stability:** `" value "`"
      next
    }
    { print }
  ' "$file" >"$file.new"
  mv "$file.new" "$file"
}

set_pair_status() {
  repo=$1
  id=$2
  value=$3
  set_status "$repo/specs/backends/$id.md" "$value"
  set_status "$repo/docs/hosts/$id.md" "$value"
}

commit_fixture() {
  repo=$1
  subject=$2
  trailer=${3-}
  git -C "$repo" add .
  if [ -n "$trailer" ]; then
    git -C "$repo" commit -q -m "$subject" \
      -m "Host-API-Stability-Transition: $trailer"
  else
    git -C "$repo" commit -q -m "$subject"
  fi
}

commit_fixture_with_body_marker() {
  repo=$1
  subject=$2
  transition=$3
  git -C "$repo" add .
  git -C "$repo" commit -q -m "$subject" \
    -m "Host-API-Stability-Transition: $transition" \
    -m 'This is ordinary message body text, not a trailer block.'
}

prepare_merge_resolution() {
  repo=$1
  branch=$2
  main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
  git -C "$repo" branch "$branch"
  printf '%s\n' "$branch main" >"$repo/$branch-main-note"
  commit_fixture "$repo" "Advance main for $branch"
  git -C "$repo" checkout -q "$branch"
  printf '%s\n' "$branch side" >"$repo/$branch-side-note"
  commit_fixture "$repo" "Advance side for $branch"
  git -C "$repo" checkout -q "$main_branch"
  git -C "$repo" merge -q --no-ff --no-commit "$branch" >/dev/null 2>&1
}

assert_merge_commit() {
  repo=$1
  label=$2
  merge_parents=$(git -C "$repo" show -s --format=%P HEAD)
  parent_count=$(printf '%s\n' "$merge_parents" | awk '{ print NF }')
  if [ "$parent_count" -ne 2 ]; then
    printf 'backend-api-stability-selftest: FAIL — %s has %s parents, expected 2\n' \
      "$label" "$parent_count" >&2
    exit 1
  fi
}

make_fixture() {
  repo=$1
  mkdir -p "$repo/ci/checks/repo-lint" "$repo/specs/backends" \
    "$repo/docs/hosts"
  cp "$LINT" "$repo/ci/checks/repo-lint/backend-api-stability.sh"
  printf '# Backends\n' >"$repo/specs/backends/README.md"
  write_backend "$repo" js evolving
  git -C "$repo" init -q
  git -C "$repo" config user.name 'Backend stability self-test'
  git -C "$repo" config user.email 'backend-stability-selftest@example.invalid'
  git -C "$repo" config diff.renames true
  commit_fixture "$repo" 'Initialize fixture'
}

expect_accept() {
  label=$1
  repo=$2
  log="$scratch/$label.log"
  if ! sh "$repo/ci/checks/repo-lint/backend-api-stability.sh" >"$log" 2>&1; then
    printf 'backend-api-stability-selftest: FAIL — rejected %s\n' "$label" >&2
    sed -n '1,160p' "$log" >&2
    selftest_failures=1
    return
  fi
  if grep -Fq 'fatal:' "$log"; then
    printf 'backend-api-stability-selftest: FAIL — %s emitted a fatal Git diagnostic\n' \
      "$label" >&2
    sed -n '1,160p' "$log" >&2
    selftest_failures=1
    return
  fi
}

expect_reject() {
  label=$1
  repo=$2
  needle=$3
  log="$scratch/$label.log"
  rc=0
  sh "$repo/ci/checks/repo-lint/backend-api-stability.sh" >"$log" 2>&1 || rc=$?
  if [ "$rc" -eq 0 ]; then
    printf 'backend-api-stability-selftest: FAIL — accepted %s\n' "$label" >&2
    sed -n '1,160p' "$log" >&2
    selftest_failures=1
    return
  fi
  if ! grep -Fq -- "$needle" "$log"; then
    printf 'backend-api-stability-selftest: FAIL — %s lacked diagnostic: %s\n' \
      "$label" "$needle" >&2
    sed -n '1,160p' "$log" >&2
    selftest_failures=1
    return
  fi
}

repo="$scratch/baseline"
make_fixture "$repo"
expect_accept baseline "$repo"

repo="$scratch/initial-rollout"
mkdir -p "$repo/ci/checks/repo-lint" "$repo/specs/backends" \
  "$repo/docs/hosts"
printf '# Backends\n' >"$repo/specs/backends/README.md"
write_backend_without_field "$repo" js
git -C "$repo" init -q
git -C "$repo" config user.name 'Backend stability self-test'
git -C "$repo" config user.email 'backend-stability-selftest@example.invalid'
git -C "$repo" config diff.renames true
commit_fixture "$repo" 'Initialize pre-policy fixture'
cp "$LINT" "$repo/ci/checks/repo-lint/backend-api-stability.sh"
write_backend "$repo" js evolving
commit_fixture "$repo" 'Roll out host API stability policy'
expect_accept initial-rollout "$repo"

repo="$scratch/invalid-value"
make_fixture "$repo"
set_pair_status "$repo" js preview
expect_reject invalid-value "$repo" 'line 3 must be exactly'

repo="$scratch/duplicate"
make_fixture "$repo"
printf "\n**Host API stability:** \`evolving\`\n" >>"$repo/specs/backends/js.md"
expect_reject duplicate "$repo" 'expected exactly one Host API stability field'

repo="$scratch/malformed-duplicate"
make_fixture "$repo"
printf '\nHost API stability: stable\n' >>"$repo/specs/backends/js.md"
expect_reject malformed-duplicate "$repo" 'expected exactly one Host API stability field'

repo="$scratch/misplaced"
make_fixture "$repo"
sed '3{h;d;};5{G;}' "$repo/specs/backends/js.md" \
  >"$repo/specs/backends/js.md.new"
mv "$repo/specs/backends/js.md.new" "$repo/specs/backends/js.md"
expect_reject misplaced "$repo" 'line 3 must be exactly'

repo="$scratch/mismatch"
make_fixture "$repo"
set_status "$repo/specs/backends/js.md" stable
expect_reject mismatch "$repo" 'specs/backends/js.md says stable but docs/hosts/js.md says evolving'

repo="$scratch/missing-guide"
make_fixture "$repo"
rm "$repo/docs/hosts/js.md"
expect_reject missing-guide "$repo" 'has no matching docs/hosts/js.md'

repo="$scratch/extra-guide"
make_fixture "$repo"
printf "# Hosting Go\n\n**Host API stability:** \`evolving\`\n" \
  >"$repo/docs/hosts/go.md"
expect_reject extra-guide "$repo" 'has no matching specs/backends/go.md'

repo="$scratch/prime"
make_fixture "$repo"
write_backend "$repo" kio-prime evolving
expect_reject prime "$repo" 'is a Kio-prime phase target, not a host backend'

repo="$scratch/new-evolving"
make_fixture "$repo"
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go backend'
expect_accept new-evolving "$repo"

repo="$scratch/new-stable"
make_fixture "$repo"
write_backend "$repo" go stable
commit_fixture "$repo" 'Add Go backend as stable'
expect_reject new-stable "$repo" 'must be introduced as evolving'

repo="$scratch/stable-pair-rename"
make_fixture "$repo"
write_backend "$repo" go evolving
i=0
while [ "$i" -lt 20 ]; do
  printf 'Backend contract detail %s.\n' "$i" \
    >>"$repo/specs/backends/js.md"
  printf 'Host guide detail %s.\n' "$i" \
    >>"$repo/docs/hosts/js.md"
  i=$((i + 1))
done
commit_fixture "$repo" 'Add Go backend'
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
mv "$repo/specs/backends/js.md" "$repo/specs/backends/ecmascript.md"
mv "$repo/docs/hosts/js.md" "$repo/docs/hosts/ecmascript.md"
set_pair_status "$repo" ecmascript evolving
commit_fixture "$repo" 'Rename and demote JavaScript backend'
rename_count=$(
  git -C "$repo" diff --name-status HEAD^ HEAD -- specs/backends docs/hosts \
    | grep -c '^R' || true
)
if [ "$rename_count" -ne 2 ]; then
  printf 'backend-api-stability-selftest: FAIL — rename fixture produced %s detected renames, expected 2\n' \
    "$rename_count" >&2
  exit 1
fi
expect_reject stable-pair-rename "$repo" 'stable backend cannot be removed before a published demotion'

repo="$scratch/rename-after-demotion"
make_fixture "$repo"
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go backend'
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Demote JavaScript backend' 'js stable -> evolving'
mv "$repo/specs/backends/js.md" "$repo/specs/backends/ecmascript.md"
mv "$repo/docs/hosts/js.md" "$repo/docs/hosts/ecmascript.md"
commit_fixture "$repo" 'Rename evolving JavaScript backend'
expect_accept rename-after-demotion "$repo"

repo="$scratch/late-field-introduction"
make_fixture "$repo"
write_backend_without_field "$repo" go
commit_fixture "$repo" 'Add Go backend without API stability'
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go API stability field late'
expect_reject late-field-introduction "$repo" 'field must be present when backend pages are introduced'

repo="$scratch/fieldless-add-delete"
make_fixture "$repo"
write_backend_without_field "$repo" go
commit_fixture "$repo" 'Add Go backend without API stability'
rm "$repo/specs/backends/go.md" "$repo/docs/hosts/go.md"
commit_fixture "$repo" 'Remove fieldless Go backend'
expect_reject fieldless-add-delete "$repo" 'field must be present when backend pages are introduced'

repo="$scratch/merged-fieldless-add-delete"
mkdir -p "$repo/ci/checks/repo-lint" "$repo/specs/backends" \
  "$repo/docs/hosts"
printf '# Backends\n' >"$repo/specs/backends/README.md"
write_backend_without_field "$repo" js
git -C "$repo" init -q
git -C "$repo" config user.name 'Backend stability self-test'
git -C "$repo" config user.email 'backend-stability-selftest@example.invalid'
git -C "$repo" config diff.renames true
commit_fixture "$repo" 'Initialize pre-policy fixture'
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" branch fieldless-side
cp "$LINT" "$repo/ci/checks/repo-lint/backend-api-stability.sh"
write_backend "$repo" js evolving
commit_fixture "$repo" 'Roll out host API stability policy'
git -C "$repo" checkout -q fieldless-side
write_backend_without_field "$repo" go
commit_fixture "$repo" 'Add fieldless Go backend on side branch'
rm "$repo/specs/backends/go.md" "$repo/docs/hosts/go.md"
commit_fixture "$repo" 'Remove fieldless Go backend on side branch'
git -C "$repo" checkout -q "$main_branch"
git -C "$repo" merge -q --no-ff -m 'Merge fieldless side history' \
  fieldless-side >/dev/null 2>&1
assert_merge_commit "$repo" merged-fieldless-add-delete
expect_reject merged-fieldless-add-delete "$repo" \
  'field must be present when backend pages are introduced'

repo="$scratch/unrecorded-transition"
make_fixture "$repo"
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend'
expect_reject unrecorded-transition "$repo" 'transition trailers do not match'

repo="$scratch/body-line-transition"
make_fixture "$repo"
set_pair_status "$repo" js stable
commit_fixture_with_body_marker "$repo" 'Promote JavaScript backend' \
  'js evolving -> stable'
expect_reject body-line-transition "$repo" 'transition trailers do not match'

repo="$scratch/wrong-transition"
make_fixture "$repo"
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js stable -> evolving'
expect_reject wrong-transition "$repo" 'transition trailers do not match'

repo="$scratch/paired-transitions"
make_fixture "$repo"
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
expect_accept recorded-promotion "$repo"
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Demote JavaScript backend' 'js stable -> evolving'
expect_accept recorded-demotion "$repo"

repo="$scratch/merge-resolution-promotion"
make_fixture "$repo"
prepare_merge_resolution "$repo" promotion-side
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript in merge resolution'
assert_merge_commit "$repo" merge-resolution-promotion
expect_reject merge-resolution-promotion "$repo" 'transition trailers do not match'

repo="$scratch/approved-merge-resolution-demotion"
make_fixture "$repo"
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
prepare_merge_resolution "$repo" approved-demotion-side
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Demote JavaScript in merge resolution' \
  'js stable -> evolving'
assert_merge_commit "$repo" approved-merge-resolution-demotion
expect_accept approved-merge-resolution-demotion "$repo"

repo="$scratch/inherited-branch-promotion"
make_fixture "$repo"
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" checkout -q -b approved-promotion
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
git -C "$repo" checkout -q "$main_branch"
printf 'main\n' >"$repo/inherited-promotion-main-note"
commit_fixture "$repo" 'Advance main before promotion merge'
git -C "$repo" merge -q --no-ff -m 'Merge approved promotion' \
  approved-promotion >/dev/null 2>&1
assert_merge_commit "$repo" inherited-branch-promotion
expect_accept inherited-branch-promotion "$repo"

repo="$scratch/inherited-branch-demotion"
make_fixture "$repo"
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" checkout -q -b approved-demotion
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Demote JavaScript backend' 'js stable -> evolving'
git -C "$repo" checkout -q "$main_branch"
printf 'main\n' >"$repo/inherited-demotion-main-note"
commit_fixture "$repo" 'Advance main before demotion merge'
git -C "$repo" merge -q --no-ff -m 'Merge approved demotion' \
  approved-demotion >/dev/null 2>&1
assert_merge_commit "$repo" inherited-branch-demotion
expect_accept inherited-branch-demotion "$repo"

repo="$scratch/stale-branch-demotion"
make_fixture "$repo"
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" branch stale-evolving
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
git -C "$repo" checkout -q stale-evolving
printf 'side\n' >"$repo/stale-evolving-side-note"
commit_fixture "$repo" 'Advance stale evolving branch'
git -C "$repo" checkout -q "$main_branch"
git -C "$repo" merge -q --no-ff --no-commit stale-evolving \
  >/dev/null 2>&1
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Resolve merge to stale evolving status'
assert_merge_commit "$repo" stale-branch-demotion
expect_reject stale-branch-demotion "$repo" 'transition trailers do not match'

repo="$scratch/partial-stable-origin-demotion"
make_fixture "$repo"
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" branch promotion-b
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript on branch A' \
  'js evolving -> stable'
git -C "$repo" branch single-origin-demotion
git -C "$repo" checkout -q promotion-b
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript on branch B' \
  'js evolving -> stable'
git -C "$repo" checkout -q "$main_branch"
git -C "$repo" merge -q --no-ff -m 'Combine independently stable branches' \
  promotion-b >/dev/null 2>&1
git -C "$repo" checkout -q single-origin-demotion
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Demote only branch A stability' \
  'js stable -> evolving'
git -C "$repo" checkout -q "$main_branch"
git -C "$repo" merge -q --no-ff --no-commit single-origin-demotion \
  >/dev/null 2>&1
commit_fixture "$repo" 'Resolve merge to partially demoted status'
assert_merge_commit "$repo" partial-stable-origin-demotion
expect_reject partial-stable-origin-demotion "$repo" \
  'transition trailers do not match'

repo="$scratch/stable-removal"
make_fixture "$repo"
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go backend'
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
rm "$repo/specs/backends/js.md" "$repo/docs/hosts/js.md"
commit_fixture "$repo" 'Remove stable JavaScript backend'
expect_reject stable-removal "$repo" 'stable backend cannot be removed before a published demotion'

repo="$scratch/merge-resolution-stable-removal"
make_fixture "$repo"
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go backend'
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
prepare_merge_resolution "$repo" removal-side
rm "$repo/specs/backends/js.md" "$repo/docs/hosts/js.md"
commit_fixture "$repo" 'Remove stable JavaScript in merge resolution'
assert_merge_commit "$repo" merge-resolution-stable-removal
expect_reject merge-resolution-stable-removal "$repo" \
  'stable backend cannot be removed before a published demotion'

repo="$scratch/inherited-branch-removal"
make_fixture "$repo"
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go backend'
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" checkout -q -b approved-removal
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Demote JavaScript backend' 'js stable -> evolving'
rm "$repo/specs/backends/js.md" "$repo/docs/hosts/js.md"
commit_fixture "$repo" 'Remove evolving JavaScript backend'
git -C "$repo" checkout -q "$main_branch"
printf 'main\n' >"$repo/inherited-removal-main-note"
commit_fixture "$repo" 'Advance main before removal merge'
git -C "$repo" merge -q --no-ff -m 'Merge approved removal' \
  approved-removal >/dev/null 2>&1
assert_merge_commit "$repo" inherited-branch-removal
expect_accept inherited-branch-removal "$repo"

repo="$scratch/merge-removal-after-branch-demotion"
make_fixture "$repo"
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go backend'
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" checkout -q -b approved-demotion
set_pair_status "$repo" js evolving
commit_fixture "$repo" 'Demote JavaScript backend' 'js stable -> evolving'
git -C "$repo" checkout -q "$main_branch"
printf 'main\n' >"$repo/merge-removal-main-note"
commit_fixture "$repo" 'Advance main before removal merge'
git -C "$repo" merge -q --no-ff --no-commit approved-demotion \
  >/dev/null 2>&1
rm "$repo/specs/backends/js.md" "$repo/docs/hosts/js.md"
commit_fixture "$repo" 'Remove demoted JavaScript in merge resolution'
assert_merge_commit "$repo" merge-removal-after-branch-demotion
expect_accept merge-removal-after-branch-demotion "$repo"

repo="$scratch/stale-branch-removal"
make_fixture "$repo"
write_backend "$repo" go evolving
commit_fixture "$repo" 'Add Go backend'
main_branch=$(git -C "$repo" symbolic-ref --short HEAD)
git -C "$repo" checkout -q -b stale-removal
rm "$repo/specs/backends/js.md" "$repo/docs/hosts/js.md"
commit_fixture "$repo" 'Remove evolving JavaScript backend'
git -C "$repo" checkout -q "$main_branch"
set_pair_status "$repo" js stable
commit_fixture "$repo" 'Promote JavaScript backend' 'js evolving -> stable'
git -C "$repo" merge -q --no-ff --no-commit stale-removal \
  >/dev/null 2>&1 || :
git -C "$repo" rev-parse -q --verify MERGE_HEAD >/dev/null
git -C "$repo" rm -q specs/backends/js.md docs/hosts/js.md
commit_fixture "$repo" 'Resolve merge by removing stable JavaScript backend'
assert_merge_commit "$repo" stale-branch-removal
expect_reject stale-branch-removal "$repo" \
  'stable backend cannot be removed before a published demotion'

repo="$scratch/split-transition"
make_fixture "$repo"
set_status "$repo/specs/backends/js.md" stable
commit_fixture "$repo" 'Split JavaScript promotion' 'js evolving -> stable'
expect_reject split-transition "$repo" 'spec/guide stability fields must change together'

repo="$scratch/shallow-source"
make_fixture "$repo"
printf 'fixture\n' >"$repo/NOTE"
commit_fixture "$repo" 'Advance fixture history'
shallow="$scratch/shallow-clone"
git clone -q --depth=1 "file://$repo" "$shallow"
expect_reject shallow-history "$shallow" 'complete Git history is required'

if [ "$selftest_failures" -ne 0 ]; then
  exit 1
fi

printf 'backend-api-stability-selftest: ok (field; mirror; rollout; default; Kio-prime exclusion; fieldless history; rename semantics; merge edges; footer trailers; transitions; stable removal; complete history)\n'
