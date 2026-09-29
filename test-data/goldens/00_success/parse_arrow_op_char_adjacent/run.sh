#!/bin/sh
# Function-arrow positions parse when the trailing `>` fuses with
# a following op-char into one greedy `SymbolRun` — `->!`, `->&`,
# `->|`, `->=`. The parser's `expect_fn_arrow` peels `->` off the
# leading bytes of the fused run via `split_current_sym(2)` at the
# function-arrow recognition sites; the residual suffix flows on
# as the next token. Each `fn` here returns a value whose right-
# hand side packs `->` flush against the next op-char.
set -u
cd workdir || exit
"$KIO_BIN" check
