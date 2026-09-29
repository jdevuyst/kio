#!/bin/sh
# Single-signature rule: each operator token sequence binds to
# exactly one function in scope. Two in-scope `op`s for `+` is
# a name conflict — exit 11 (parse error).
set -u
cd workdir || exit
"$KIO_BIN" check
