#!/bin/sh
set -eu

# Type-chain separator comments survive formatting and reach a fixpoint.
formatted=$("$KIO_BIN" fmt - <<'KIO'
module type_chain_comments;

fn product[A][B](f: (A & B) -> A) -> (& A // product domain
& B) -> A { f }

fn sum[A][B](f: (A | B) -> B) -> (A // sum domain
| B) -> B { f }

type Nested[A][B][C] = A // outer operand
& (B // inner operand
& C);

type Applied[*F][A][B] = A // before application
| F( // type argument
  B // closing argument
  );
KIO
)
again=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
if [ "$formatted" != "$again" ]; then
  printf '%s\n' 'formatter output was not idempotent' >&2
  exit 1
fi
printf '%s\n' "$formatted"
