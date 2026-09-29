#!/bin/sh
# `onto!` accepts identity when target DNF has a branch whose
# factor multi-set is a strict subset of a later branch —
# `[String]` is a strict subset of `[String, I32]`. The engine's
# saturating-first per-source-branch preference picks the exact
# target branch for `(String & I32)` rather than catching it in
# the earlier `String` branch via projection.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
