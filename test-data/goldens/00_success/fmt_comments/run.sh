#!/bin/sh
# Pin trivia preservation for top-level item / import leading
# comments. The source carries:
# - A comment above an `import` statement.
# - Two paragraphs of comments above `id`, separated by a blank
#   line that should survive the format round-trip.
# - A run of empty `//` lines surrounding a comment, which should
#   collapse to a single empty `//` between the surviving comment.
# - A simple comment above `last`.
#
# This case lives in the corpus as a regression guard against the
# trivia-walker changes — `kio fmt` should round-trip every comment
# at the right indent and with the right blank-line spacing.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
