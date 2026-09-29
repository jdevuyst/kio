#!/bin/sh
# `ease!`'s function-arrow walk rejects polymorphic function
# sources or targets. Coercing under a `[A]`-binder needs
# instantiation machinery the elaborator doesn't have — out of scope by
# spec. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
