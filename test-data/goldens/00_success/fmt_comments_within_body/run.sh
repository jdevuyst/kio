#!/bin/sh
# Pin trivia preservation for the within-body positions added in
# the comment-dropping fix:
# - Between `=` and the value of a `let` binding.
# - Between `in` and the body of a `let` binding (when the body
#   isn't itself a `let` with its own leading-trivia slot).
# - Inside a call's argument list — for both value-args and
#   type-args. Forces multi-line leading-comma layout when any arg
#   carries a leading comment.
# - Inside a `match!` clause body (between `{` and the body).
# - Inside a tuple literal `(a, b)` — comments above any element.
# - Inside a label-value `{f = e, g = e'}` — comments above any
#   label.
# - Inside a type-application argument list `Foo(A, B)` — comments
#   above any type-arg, including when calling a polymorphic newtype
#   member with type-args interleaved with value-args.
# - Inside a `labels { f: X, g: Y }` block — comments above any
#   label entry.
#
# This case lives in the corpus as a regression guard: every comment
# in the source must round-trip through `kio fmt` at the right
# indent and in the right place. The case runs the formatter twice
# to assert idempotence.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
