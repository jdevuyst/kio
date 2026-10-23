#!/bin/sh
# `fit!`'s function-arrow walk rejects polymorphic source / target
# at a structural pre-condition — the spine engine has no
# instantiation machinery. Source is `[A].A -> A`, target is
# `A -> A`. fit! refuses to enter the binder. Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
