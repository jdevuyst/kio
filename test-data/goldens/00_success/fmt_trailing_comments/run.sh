#!/bin/sh
# Regression guard for the dropped trailing / dangling `//` comment
# bug: a comment in a closing-delimiter or end-of-file position used
# to vanish on a `kio fmt` round-trip because trivia is leading-only.
# Each `fn` here puts a comment at one of the formerly-dropped sites:
# - `let y = x; // bind y` and `y // the result` — trailing on a
#   statement / final expression before `}`.
# - the dangling comment after the last call arg, before `)`.
# - the trailing comment on the last tuple element, before `)`.
# - a comment dangling at the end of a block / `do` block, before `}`.
# - `fn last() -> . { () } // ...` — trailing after the last item,
#   before end-of-file.
#
# Strategy mirrors fmt_comments_within_body: the source is copied to
# scratch so `kio fmt` (which rewrites in place) doesn't canonicalise
# the tracked, deliberately non-canonical source (hence the
# `SKIP_KIO_FMT_CHECK` marker). The case runs the formatter twice and
# cats both times to assert the relocated layout is idempotent.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
