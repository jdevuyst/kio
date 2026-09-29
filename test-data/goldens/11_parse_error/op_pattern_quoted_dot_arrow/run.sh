#!/bin/sh
# Dot-led one-dot op tokens stay reserved even when quoted. `(.>)`
# remains held back for UFCS dispatch and future dot-led syntax;
# dot-leading operator tokens must contain at least two dots (`(..)`,
# `(.+.)`).
set -u
cd workdir || exit
"$KIO_BIN" check
