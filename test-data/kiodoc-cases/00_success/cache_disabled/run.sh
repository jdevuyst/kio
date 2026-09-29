#!/bin/sh
# `cache ();` opts the package out of every kio on-disk cache.
# `kio doc` runs the snippet uncached and creates no cache
# directory.
set -eu

"$KIO_BIN" doc check

# No cache directory must be materialized on the cwd's tree.
if [ -d out ]; then
  # shellcheck disable=SC2016 # literal backticks in user message
  printf 'expected no `out/` directory; found:\n' >&2
  find out -maxdepth 3 >&2 || true
  exit 1
fi
