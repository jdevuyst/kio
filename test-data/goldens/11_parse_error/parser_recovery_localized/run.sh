#!/bin/sh
# A fn body opens with `{` but never closes before the next
# top-level `fn`. The tree-skeleton CST captures the group as
# `recovered = true`, and the parser surfaces a localized parse
# error scoped to the group rather than cascading through the
# rest of the file. Exit 11.
set -u
cd workdir || exit
"$KIO_BIN" check
