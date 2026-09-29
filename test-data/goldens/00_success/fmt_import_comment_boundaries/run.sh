#!/bin/sh
set -eu

# Sorting imports preserves statement, item and closing-boundary comments.
formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_import_comment_boundaries;
// statement
import z( // opening selection
  , // zebra
    zebra
  , // alpha
    alpha
  // closing
  );
import a as a;
KIO
)
again=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
if [ "$formatted" != "$again" ]; then
  printf '%s\n' 'formatter output was not idempotent' >&2
  exit 1
fi
printf '%s\n' "$formatted"
