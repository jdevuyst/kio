#!/bin/sh
# Regression guard for a `kio fmt` comment-duplication bug.
#
# Shape: a `//` comment immediately before a `let` whose RHS is a
# multi-line block, inside a closure passed as a comma-prefixed
# positional argument (see workdir/main.kio's `swap`). The lambda literal's
# block-body parser captured the trivia between `{` and the first
# statement, and the inner `let` captured the same trivia onto its own
# `meta.leading_trivia` — so the comment landed twice and `kio fmt`
# emitted it twice. The fix skips the `fn`-body capture when the body
# is a closure `let`/`Seq`.
#
# The source on disk is already at `kio fmt`'s fixed point, so the
# automatic per-case fmt-canonical check (no SKIP_KIO_FMT_CHECK here)
# asserts idempotence — the duplicated-comment regression would make
# the canonical form fail to round-trip. This run.sh pins the same
# invariant directly: `fmt --check` exits 0 on the canonical source,
# and the program typechecks.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" fmt --check main.kio || exit
"$KIO_BIN" check
