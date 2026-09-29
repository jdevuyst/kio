#!/bin/sh
# An `op` operator component whose spelling starts with `//` is
# reserved for future syntax — the whole `//…` family. The lexer caps
# a single `SymbolRun` at one `/` (a second `/` opens a comment), so a
# `//` prefix is reachable only as two adjacent `/` op-tokens
# (`op _ / / _ { impl f, };`); the parser joins the run and rejects it. This
# is a `starts_with("//")` prefix reservation, distinct from the
# leading-dot one-dot reservation. Exit code 11 (parse error).
set -u
cd workdir || exit
"$KIO_BIN" check
