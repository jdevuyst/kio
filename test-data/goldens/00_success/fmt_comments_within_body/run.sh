#!/bin/sh
# Pin trivia preservation for the within-body positions added in
# the comment-dropping fix:
# - Between `=` and the value of a `let` binding.
# - Before the continuation of a `let` binding (when the body
#   isn't itself a `let` with its own leading-trivia slot).
# - Inside a call's argument list — for both value-args and
#   type-args. Forces multi-line leading-comma layout when any arg
#   carries a leading comment.
# - Inside a `match` clause body (between `{` and the body).
# - Inside a tuple literal `(a, b)` — comments above any element.
# - Inside a row-construction macro block — comments above each
#   nominal constructor call.
# - Inside singleton type-application groups `Foo[A][B]` — comments
#   above each type argument, including on a polymorphic nominal member
#   with multiple type stages before its value packet.
# - Inside an ordinary type alias product — comments above each
#   referenced nominal type.
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
