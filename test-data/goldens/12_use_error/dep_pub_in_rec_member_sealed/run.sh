#!/bin/sh
# A `pub(P)`-scoped `rec`-group member of a path dependency, re-rooted
# under the consumer, stays sealed — a consumer outside its subtree cannot
# import it (exit 12).
#
# The dependency `seclib` declares `spin` as `pub(calc)` inside a
# `rec(loop)` group. Materialization re-roots the scope to
# `pub(seclib/calc)` (committed under `workdir/seclib/`), and the
# `rec`-group desugar preserves the scope onto the wrapper `fn`. So the
# consumer's `main`, which is not within `seclib/calc`, is sealed out of
# importing `spin`.
#
# Before the fix the scope was dropped at desugar (collapsed to a bare
# `pub`), so this import wrongly succeeded; the seal is the regression
# subject.
set -u
cd workdir || exit
"$KIO_BIN" check
