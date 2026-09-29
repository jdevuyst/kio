#!/bin/sh
# `()` is always the Unit value. It cannot occupy `identity`'s type slot;
# callers that spell the Unit type explicitly use `.`.
set -u
cd workdir || exit
"$KIO_BIN" check
