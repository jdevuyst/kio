#!/bin/sh
# Existential-binder closer `>` parses even when greedy lexer
# fusion absorbed it into a longer `SymbolRun`. The parser's
# `expect_existential_close` peels `>` off the leading byte via
# `split_current_sym(1)` at the existential-binder close site,
# and the residual (`:` after `<U>`, `<` after the first `<U>`
# in chained `<U><V>`) flows on as the next token. Pinned in
# both `labels` entries and `newtype` headers.
set -u
cd workdir || exit
"$KIO_BIN" check
