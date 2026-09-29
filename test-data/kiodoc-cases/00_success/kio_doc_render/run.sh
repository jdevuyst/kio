#!/bin/sh
# End-to-end `kio doc build` golden.
#
# The case is a package with a root module, two child modules (one nested),
# a package boundary, `///` doc-comments carrying intra-doc links and an
# @signature directive, and two tutorial pages (one nested). It
# renders both formats and byte-checks the rendered output.
#
# `kio doc build` first runs `kio doc check`; a check failure would
# abort before rendering. The case asserts on the rendered Markdown
# and HTML *page* files — the `_assets/` stylesheet / script bundle
# is a large static blob and is excluded from the byte-golden.
set -eu

trap 'rm -rf out' EXIT

"$KIO_BIN" doc build --md --html

# Emit every rendered page, sorted, with a path banner before each,
# so the run-tests diff harness byte-checks the rendered output.
# `_assets/` is skipped — it's the fixed stylesheet / script bundle.
find out -type f \( -name '*.md' -o -name '*.html' \) \
  | grep -v '/_assets/' \
  | LC_ALL=C sort \
  | while IFS= read -r page; do
      if [ "${first_page:-1}" = 0 ]; then
        printf '\n'
      fi
      first_page=0
      printf '==== %s ====\n' "$page"
      cat "$page"
    done
