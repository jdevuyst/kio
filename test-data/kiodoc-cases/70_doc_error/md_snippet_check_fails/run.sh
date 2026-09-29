#!/bin/sh
# A harness-wrapped `.md` snippet that `kio check` rejects while
# asserting the default `check_exit_code=0`. The everyday failure: the
# author wrote a snippet that does not typecheck.
#
# Sibling `check_exit_mismatch` covers the converse — a snippet that
# typechecks but declares a non-zero code — where there is no `kio
# check` diagnostic to show. Here there is one, and it must reach the
# author: under its own header, after the correlated "file, line,
# expected vs actual" message rather than ahead of it, and followed by
# the assembled source.
#
# The diagnostic cites the per-snippet scratch package, whose path
# carries a nonce, so this case pins stderr by `.grep` rather than
# byte-exactly.
set -u
"$KIO_BIN" doc check
