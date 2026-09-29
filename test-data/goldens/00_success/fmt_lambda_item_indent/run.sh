#!/bin/sh
# Regression guard for a `kio fmt` A1 item-indentation bug.
#
# Shape: a lambda with a block body passed as an item of a broken
# leading-comma argument list (see workdir/main.kio's `keep_matching`).
# The broken A1 layout used to render a multi-line item's continuation
# lines relative to the comma column rather than the item's own content
# column: the lambda's `if` body sat level with the lambda header and
# the lambda's closing `}` level with the list's commas and closer. The
# fix nests each broken-layout item by the width of its `", "` prefix,
# so the block body sits at +2 from the lambda header and the `}`
# returns to the item's content column.
#
# The source on disk is already at `kio fmt`'s fixed point, so the
# automatic per-case fmt-canonical check (no SKIP_KIO_FMT_CHECK here)
# asserts idempotence — a regression to the comma-column anchoring
# would make the canonical form fail to round-trip. This run.sh pins
# the same invariant directly: `fmt --check` exits 0 on the canonical
# source, and the program typechecks.
set -u
cd workdir || exit
"$KIO_BIN" fmt --check main.kio || exit
"$KIO_BIN" check
