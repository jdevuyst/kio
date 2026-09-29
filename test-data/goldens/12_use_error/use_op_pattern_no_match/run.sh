#!/bin/sh
# `import use_op_pattern_no_match/helper(op _ ??);` is an import error
# (exit 12) when the helper exports no matching public operator grammar.
set -u
cd workdir || exit
"$KIO_BIN" check
