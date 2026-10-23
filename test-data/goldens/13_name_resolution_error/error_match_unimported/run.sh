#!/bin/sh
# The `match` macro called with no `import <module>(macro match);`
# declaration. Like the structural macros, `match` is an ordinary
# imported member; the call site is the standard unresolved-name error
# (exit 13), naming the import line to add.
set -u
cd workdir || exit
"$KIO_BIN" check
