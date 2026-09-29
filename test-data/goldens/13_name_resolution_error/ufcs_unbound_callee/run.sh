#!/bin/sh
# UFCS dispatch with an unbound callee fires the standard
# name-resolution diagnostic on the callee's path — exit 13.
# `expected.stderr` is pinned (not ignored) to lock the caret/snippet
# layout from `specs/diagnostics.md`: the `path:line:col: error: …`
# header, the ` | ` gutter rail, the offending source line, and the
# `^` caret run sitting under the whole unbound callee path. NO_COLOR
# / non-TTY capture keeps the output plain for the byte-equal diff.
set -u
cd workdir || exit
"$KIO_BIN" check
