#!/bin/sh
# Dot-led one-dot op tokens stay reserved even when quoted. Unlike
# `=` (which lifts to a user op via `op _ (=) _ { impl my_eq, };`), quotation
# does not admit `(.)`; dot-leading operator tokens must contain at
# least two dots (`(..)`, `(.+.)`).
set -u
cd workdir || exit
"$KIO_BIN" check
