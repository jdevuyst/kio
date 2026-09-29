#!/bin/sh
set -eu

# Comments before an operator remain after its operand's internal comments.
# This is a formatter-only source round trip, independent of host backends.
formatted=$("$KIO_BIN" fmt - <<'KIO'
module operator_comments;

pure fn pick(x: ., y: .) -> . { x }
op _ <+> _ { impl pick }

fn example() -> . {
  .() {
    // inside operand
    ()
  }() // before operator
    <+> ()
}
KIO
)
again=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
if [ "$formatted" != "$again" ]; then
  printf '%s\n' 'formatter output was not idempotent' >&2
  exit 1
fi
printf '%s\n' "$formatted"
