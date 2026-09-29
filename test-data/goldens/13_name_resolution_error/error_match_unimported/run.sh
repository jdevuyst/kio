#!/bin/sh
# The `match!` elaborator called with no `import <module>(match);`
# declaration. Like the structural elaborators, `match!` is an ordinary
# imported member; the call site is the standard unresolved-name error
# (exit 13), naming the import line to add.
set -u
cd workdir || exit
"$KIO_BIN" check
