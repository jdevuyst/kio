#!/bin/sh
# `&` and `|` join op-runs when adjacent to another op-char, so
# pairs like `&&` / `&&--` (and `||` / `||=`) lex as disjoint
# single tokens at the trie level — they coexist instead of
# hitting the prefix-forbidden conflict that fires on
# whitespace-separated `[&, &]` vs `[&, &, --]` shapes.
set -u
cd workdir || exit
"$KIO_BIN" check
