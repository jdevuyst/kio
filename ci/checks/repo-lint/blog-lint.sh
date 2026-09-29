#!/bin/sh
#
# Lint the hand-written blog posts under docs/blog/. Each post must:
#   - be named YYYY-MM-DD-<slug>.md with a valid calendar-ish date. The
#     date drives ordering and the published URL; a mis-named file is
#     silently never published, so a wrong name is an error, not a skip.
#   - carry exactly one H1 title (the generator takes the post title from it);
#   - carry a byline in the standard attribution style, linking to a GitHub
#     profile:  _By [Name](https://github.com/<user>)_
#   - pin every GitHub link to a file or directory in this repository to a
#     full commit object ID rather than a moving branch or tag.
#
# These are the authoring invariants website/scripts/prepare-docs.mjs relies
# on to render a post's title, date, and author. Static check, no build.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

BLOG_DIR="$REPO_ROOT/docs/blog"

[ -d "$BLOG_DIR" ] || exit 0

status=0
fail() {
  printf 'blog-lint: %s\n' "$1" >&2
  status=1
}

for post in "$BLOG_DIR"/*.md; do
  [ -e "$post" ] || continue
  name=$(basename "$post")

  if ! printf '%s\n' "$name" \
    | grep -qE '^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])-.+\.md$'; then
    fail "docs/blog/$name must be named YYYY-MM-DD-<slug>.md with a valid date"
    continue
  fi

  h1_count=$(grep -cE '^# ' "$post" || true)
  [ "$h1_count" -ge 1 ] || fail "docs/blog/$name has no H1 title"
  [ "$h1_count" -le 1 ] || fail "docs/blog/$name has more than one H1 title"

  grep -qE '^_By \[[^]]+\]\(https?://(www\.)?github\.com/[^)]+\)_[[:space:]]*$' "$post" \
    || fail "docs/blog/$name has no standard byline: _By [Name](https://github.com/<user>)_"

  link_errors=$(
    awk -v file="docs/blog/$name" '
      function scan(line, url_pattern, prefix_pattern,    rest, url, ref) {
        rest = line
        while (match(rest, url_pattern)) {
          url = substr(rest, RSTART, RLENGTH)
          ref = url
          sub(prefix_pattern, "", ref)
          if (length(ref) != 40 || ref !~ /^[0-9a-f]+$/) {
            printf "blog-lint: %s:%d repository content link uses ref `%s`; use a full 40-character commit id\n", file, FNR, ref
          }
          rest = substr(rest, RSTART + RLENGTH)
        }
      }

      {
        scan($0,
          "https://github\\.com/jdevuyst/kio/(blob|tree)/[^/[:space:])]+",
          "^https://github\\.com/jdevuyst/kio/(blob|tree)/")
        scan($0,
          "https://raw\\.githubusercontent\\.com/jdevuyst/kio/[^/[:space:])]+",
          "^https://raw\\.githubusercontent\\.com/jdevuyst/kio/")
      }
    ' "$post"
  )
  if [ -n "$link_errors" ]; then
    printf '%s\n' "$link_errors" >&2
    status=1
  fi
done

exit "$status"
