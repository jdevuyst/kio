#!/bin/sh
# A doc-comment (`///`) immediately before an `import` clause is a parse error.
# `import` clauses are not documented; doc-comments must immediately precede
# a top-level definition (`fn`, `type`, `literal`, `labels`, or `op`). The parser
# rejects this with exit code 11 (parse error).
set -u
cd workdir || exit
"$KIO_BIN" check
