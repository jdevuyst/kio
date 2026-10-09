#!/bin/sh
# Exercise the blog authoring checks against disposable post fixtures.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/blog-lint-selftest.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
mkdir -p "$scratch/repo/ci/checks/repo-lint" "$scratch/repo/docs/blog"
cp "$SCRIPT_DIR/blog-lint.sh" "$scratch/repo/ci/checks/repo-lint/blog-lint.sh"
post="$scratch/repo/docs/blog/2026-01-01-fixture.md"

write_post() {
  printf '%s\n' '# Fixture' '' '_By [Writer](https://github.com/writer)_' '' "$1" > "$post"
}

expect() {
  wanted=$1
  diagnostic=${2:-}
  got=0
  sh "$scratch/repo/ci/checks/repo-lint/blog-lint.sh" > "$scratch/output" 2>&1 || got=$?
  if [ "$got" -ne "$wanted" ]; then
    printf 'blog-lint-selftest: expected exit %s, got %s\n' "$wanted" "$got" >&2
    cat "$scratch/output" >&2
    exit 1
  fi
  if [ -n "$diagnostic" ]; then
    if ! grep -Fq "$diagnostic" "$scratch/output"; then
      printf 'blog-lint-selftest: missing diagnostic: %s\n' "$diagnostic" >&2
      cat "$scratch/output" >&2
      exit 1
    fi
  elif [ -s "$scratch/output" ]; then
    printf 'blog-lint-selftest: unexpected output for valid post\n' >&2
    cat "$scratch/output" >&2
    exit 1
  fi
}

write_post "The writer's \"example\" — café, 日本語, and ™."
expect 0

# Each character must be found by itself, not only as a matched quotation pair.
for quote in "‘" "’" '“' '”'; do
  write_post "A ${quote} quotation."
  expect 1 'docs/blog/2026-01-01-fixture.md:5 use straight ASCII quotation marks and apostrophes'
done

write_post "The writer’s example."
expect 1 'docs/blog/2026-01-01-fixture.md:5 use straight ASCII quotation marks and apostrophes'

# UTF-8 byte matching must not reject unrelated punctuation in a C locale.
write_post 'An em dash — and an ellipsis … are valid.'
LC_ALL=C expect 0
write_post 'A “quotation”.'
LC_ALL=C expect 1 'docs/blog/2026-01-01-fixture.md:5 use straight ASCII quotation marks and apostrophes'

write_post '[file](https://github.com/jdevuyst/kio/blob/0123456789012345678901234567890123456789/README.md)'
expect 0
write_post '[directory](https://github.com/jdevuyst/kio/tree/main/docs)'
expect 1 'repository content link uses ref'
write_post '“Quote” and [file](https://github.com/jdevuyst/kio/blob/main/README.md)'
expect 1 'use straight ASCII quotation marks and apostrophes'
grep -Fq 'repository content link uses ref' "$scratch/output"

write_post '# Second title'
expect 1 'has more than one H1 title'
printf '%s\n' '# No byline' > "$post"
expect 1 'has no standard byline'

write_post 'Valid text.'
mv "$post" "$scratch/repo/docs/blog/undated.md"
expect 1 'must be named YYYY-MM-DD-<slug>.md'

printf 'blog-lint-selftest: ok\n'
