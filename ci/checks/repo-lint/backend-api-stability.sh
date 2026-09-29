#!/bin/sh
#
# Verify the host-backend API stability publication contract.
#
# The current-tree pass derives the host backend set from specs/backends/*.md,
# requires a matching docs/hosts page, and checks the one canonical field and
# its mirrored value. The history pass makes `evolving` the introduction
# default, treats a page rename as removal plus a new backend, and requires
# every later value change to be paired in one commit and declared by an exact
# footer trailer:
#
#   Host-API-Stability-Transition: <backend> <old> -> <new>
#
# The trailer makes the transition explicit and mechanically reviewable. A
# merge may inherit an already-validated state from one parent; a state made
# only by merge resolution is checked against every parent. User approval
# remains an interaction-level authority gate enforced by AGENTS.md.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

failures=0

fail() {
  printf 'backend-api-stability: %s\n' "$1" >&2
  failures=1
}

current_field() {
  file=$1
  count=$(grep -c 'Host API stability:' "$file" || true)
  if [ "$count" -ne 1 ]; then
    printf 'backend-api-stability: %s: expected exactly one Host API stability field, found %s\n' \
      "$file" "$count" >&2
    return 1
  fi

  line=$(sed -n '3p' "$file")
  case "$line" in
    "**Host API stability:** \`evolving\`") printf '%s\n' evolving ;;
    "**Host API stability:** \`stable\`") printf '%s\n' stable ;;
    *)
      printf "backend-api-stability: %s: line 3 must be exactly **Host API stability:** \`evolving\` or **Host API stability:** \`stable\`\n" \
        "$file" >&2
      return 1
      ;;
  esac
}

spec_ids=$(
  for file in specs/backends/*.md; do
    [ -f "$file" ] || continue
    [ "$file" = specs/backends/README.md ] && continue
    basename -- "$file" .md
  done | LC_ALL=C sort
)

doc_ids=$(
  for file in docs/hosts/*.md; do
    [ -f "$file" ] || continue
    [ "$file" = docs/hosts/README.md ] && continue
    basename -- "$file" .md
  done | LC_ALL=C sort
)

if [ -z "$spec_ids" ]; then
  fail 'no host backend pages found under specs/backends/'
fi

backend_count=0
for id in $spec_ids; do
  backend_count=$((backend_count + 1))
  case "$id" in
    prime|kio-prime|kio_prime)
      fail "$id is a Kio-prime phase target, not a host backend"
      ;;
  esac

  spec="specs/backends/$id.md"
  guide="docs/hosts/$id.md"
  if [ ! -f "$guide" ]; then
    fail "$spec has no matching $guide"
    continue
  fi

  spec_value=$(current_field "$spec") || {
    failures=1
    continue
  }
  guide_value=$(current_field "$guide") || {
    failures=1
    continue
  }
  if [ "$spec_value" != "$guide_value" ]; then
    fail "$spec says $spec_value but $guide says $guide_value"
    continue
  fi
  printf 'backend-api-stability: %-12s %s\n' "$id" "$spec_value"
done

for id in $doc_ids; do
  if [ ! -f "specs/backends/$id.md" ]; then
    fail "docs/hosts/$id.md has no matching specs/backends/$id.md"
  fi
done

if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
  fail 'complete Git history is required to verify status introductions and transitions'
fi

empty_tree=$(git hash-object -t tree /dev/null)

blob_field() {
  tree=$1
  file=$2
  if ! git cat-file -e "$tree:$file" 2>/dev/null; then
    printf '%s\n' @absent
    return
  fi

  line=$(git show "$tree:$file" | sed -n '3p')
  case "$line" in
    "**Host API stability:** \`evolving\`") printf '%s\n' evolving ;;
    "**Host API stability:** \`stable\`") printf '%s\n' stable ;;
    *) printf '%s\n' @missing-or-invalid ;;
  esac
}

# The first addition of this gate identifies the one policy-rollout commit
# allowed to retrofit `evolving` onto backend pages that already existed. The
# identity is structural, so rebasing or cherry-picking the rollout does not
# require a hard-coded commit hash.
if ! rollout_history=$(git log -m --full-history --no-renames --reverse \
  --diff-filter=A --format=%H -- \
  ci/checks/repo-lint/backend-api-stability.sh); then
  fail 'cannot read history for the Host API stability policy rollout'
  exit 1
fi
rollout_commit=$(printf '%s\n' "$rollout_history" | sed -n '1p')

if [ -z "$rollout_commit" ]; then
  fail 'cannot identify the initial Host API stability policy rollout'
  exit 1
fi

rollout_parents=$(git show -s --format=%P "$rollout_commit")

# The rollout's parent ancestry is the complete pre-policy baseline. Any other
# commit first enters published history with or after the rollout and is
# checked, including a side branch later merged without a surviving tree diff.
is_policy_history_commit() {
  candidate=$1
  for baseline_tip in $rollout_parents; do
    if git merge-base --is-ancestor "$candidate" "$baseline_tip"; then
      return 1
    fi
  done
  return 0
}

backend_state() (
  tree=$1
  id=$2
  state_spec=$(blob_field "$tree" "specs/backends/$id.md")
  state_guide=$(blob_field "$tree" "docs/hosts/$id.md")
  if [ "$state_spec" = "$state_guide" ]; then
    printf '%s\n' "$state_spec"
  else
    printf '%s\n' @mismatch
  fi
)

# List only commits whose separate-parent diff changes a stability field for
# this backend. `-m` keeps merge-resolution changes visible, and bounding the
# provenance walk to these events avoids recursing through unrelated commits.
status_event_commits() {
  tip=$1
  id=$2
  raw_events=$(git log -m --full-history --no-renames --format=%H \
    -G'^\*\*Host API stability:\*\*' "$tip" -- \
    "specs/backends/$id.md" "docs/hosts/$id.md") || return 1
  printf '%s\n' "$raw_events" | sed '/^$/d' | LC_ALL=C sort -u
}

# Return the status events nearest to `tip` in the commit DAG. Multiple
# incomparable events remain visible, which preserves independently promoted
# stable origins across a merge.
nearest_status_events() (
  tip=$1
  id=$2
  events=$(status_event_commits "$tip" "$id")
  for event in $events; do
    git merge-base --is-ancestor "$event" "$tip" || continue
    shadowed=false
    for later in $events; do
      [ "$later" = "$event" ] && continue
      git merge-base --is-ancestor "$later" "$tip" || continue
      if git merge-base --is-ancestor "$event" "$later"; then
        shadowed=true
        break
      fi
    done
    [ "$shadowed" = false ] && printf '%s\n' "$event"
  done
)

# Return success only when `source` descends from every origin of the active
# stable epoch at `node`. Stable merge events recurse through stable parents;
# transition candidates, rather than unrelated commits, bound the walk.
all_stable_origins_reach_source() (
  node=$1
  source=$2
  id=$3
  [ "$(backend_state "$node" "$id")" = stable ] || return 1

  events=$(nearest_status_events "$node" "$id")
  [ -n "$events" ] || return 1
  for event in $events; do
    [ "$(backend_state "$event" "$id")" = stable ] || return 1
    stable_parent=false
    for event_parent in $(git show -s --format=%P "$event"); do
      if [ "$(backend_state "$event_parent" "$id")" = stable ]; then
        stable_parent=true
        all_stable_origins_reach_source \
          "$event_parent" "$source" "$id" || return 1
      fi
    done
    if [ "$stable_parent" = false ]; then
      git merge-base --is-ancestor "$event" "$source" || return 1
    fi
  done

  return 0
)

state_is_inherited() {
  commit=$1
  edge_parent=$2
  id=$3
  old_state=$4
  spec_state=$5
  guide_state=$6

  for source_parent in $(git show -s --format=%P "$commit"); do
    [ "$source_parent" = "$edge_parent" ] && continue
    is_policy_history_commit "$source_parent" || continue
    source_spec=$(blob_field "$source_parent" "specs/backends/$id.md")
    source_guide=$(blob_field "$source_parent" "docs/hosts/$id.md")
    case "$old_state:$spec_state" in
      stable:evolving)
        if [ "$source_spec" != evolving ] || [ "$source_guide" != evolving ]; then
          continue
        fi
        all_stable_origins_reach_source \
          "$edge_parent" "$source_parent" "$id" || continue
        ;;
      stable:@absent)
        case "$source_spec:$source_guide" in
          evolving:evolving|@absent:@absent) ;;
          *) continue ;;
        esac
        all_stable_origins_reach_source \
          "$edge_parent" "$source_parent" "$id" || continue
        ;;
      *)
        if [ "$source_spec" != "$spec_state" ] || \
           [ "$source_guide" != "$guide_state" ]; then
          continue
        fi
        ;;
    esac
    return 0
  done

  return 1
}

if ! path_history=$(git log -m --full-history --no-renames --format=%H -- \
  specs/backends docs/hosts); then
  fail 'cannot read backend-page history'
  exit 1
fi
if ! trailer_history=$(git log --format=%H \
  --grep='Host-API-Stability-Transition:'); then
  fail 'cannot read transition-trailer history'
  exit 1
fi
history_commits=$(
  printf '%s\n%s\n' "$path_history" "$trailer_history" \
    | sed '/^$/d' \
    | LC_ALL=C sort -u
)

for commit in $history_commits; do
  if ! is_policy_history_commit "$commit"; then
    continue
  fi

  expected=
  parents=$(git show -s --format=%P "$commit")
  if [ -z "$parents" ]; then
    parents=$empty_tree
  fi

  for parent in $parents; do
    if ! changed_paths=$(git diff --no-renames --name-only \
      "$parent" "$commit" -- specs/backends docs/hosts); then
      fail "$commit: cannot inspect backend-page changes against parent $parent"
      continue
    fi
    changed_ids=$(
      printf '%s\n' "$changed_paths" \
        | while IFS= read -r file; do
            case "$file" in
              specs/backends/README.md|docs/hosts/README.md) ;;
              specs/backends/*.md|docs/hosts/*.md)
                basename -- "$file" .md
                ;;
            esac
          done \
        | LC_ALL=C sort -u
    )

    for id in $changed_ids; do
      old_spec=$(blob_field "$parent" "specs/backends/$id.md")
      old_guide=$(blob_field "$parent" "docs/hosts/$id.md")
      new_spec=$(blob_field "$commit" "specs/backends/$id.md")
      new_guide=$(blob_field "$commit" "docs/hosts/$id.md")

      if [ "$old_spec" != "$old_guide" ]; then
        fail "$commit ($id): parent spec/guide stability fields disagree ($old_spec vs $old_guide)"
        continue
      fi
      if [ "$new_spec" != "$new_guide" ]; then
        fail "$commit ($id): spec/guide stability fields must change together ($new_spec vs $new_guide)"
        continue
      fi

      case "$new_spec" in
        evolving|stable|@absent)
          if [ "$parent" != "$empty_tree" ] && \
             state_is_inherited "$commit" "$parent" "$id" \
               "$old_spec" "$new_spec" "$new_guide"; then
            continue
          fi
          ;;
      esac

      case "$old_spec:$new_spec" in
        @absent:@absent|evolving:@absent)
          ;;
        stable:@absent)
          fail "$commit ($id): a stable backend cannot be removed before a published demotion to evolving"
          ;;
        @absent:evolving)
          ;;
        @missing-or-invalid:evolving)
          if [ "$commit" != "$rollout_commit" ]; then
            fail "$commit ($id): the Host API stability field must be present when backend pages are introduced"
          fi
          ;;
        @absent:stable|@missing-or-invalid:stable)
          fail "$commit ($id): a Host API stability field must be introduced as evolving"
          ;;
        evolving:evolving|stable:stable)
          ;;
        evolving:stable|stable:evolving)
          transition="$id $old_spec -> $new_spec"
          if [ -n "$expected" ]; then
            expected="$expected
$transition"
          else
            expected=$transition
          fi
          ;;
        @absent:@missing-or-invalid)
          fail "$commit ($id): the Host API stability field must be present when backend pages are introduced"
          ;;
        *:@missing-or-invalid)
          fail "$commit ($id): an existing backend page lost or malformed its Host API stability field"
          ;;
        *)
          fail "$commit ($id): invalid Host API stability history state $old_spec -> $new_spec"
          ;;
      esac
    done
  done

  expected_sorted=$(printf '%s\n' "$expected" | sed '/^$/d' | LC_ALL=C sort -u)
  if ! commit_message=$(git show -s --format=%B "$commit"); then
    fail "$commit: cannot read commit message"
    continue
  fi
  if ! parsed_trailers=$(printf '%s\n' "$commit_message" \
    | git -c trailer.separators=: interpret-trailers --parse --no-divider); then
    fail "$commit: cannot parse commit trailers"
    continue
  fi
  trailer_values=$(printf '%s\n' "$parsed_trailers" \
    | sed -n 's/^Host-API-Stability-Transition:[[:space:]]*//p')
  trailers=$(printf '%s\n' "$trailer_values" | sed '/^$/d' | LC_ALL=C sort)
  if [ "$trailers" != "$expected_sorted" ]; then
    short=$(git rev-parse --short "$commit")
    fail "$short: transition trailers do not match the paired field changes (expected: ${expected_sorted:-none}; found: ${trailers:-none})"
  fi
done

if [ "$failures" -ne 0 ]; then
  exit 1
fi

printf 'backend-api-stability: OK (%s host backends)\n' "$backend_count"
