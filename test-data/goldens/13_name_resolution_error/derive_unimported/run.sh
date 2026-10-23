#!/bin/sh
# `derive` called with no `import <module>(macro derive);` declaration. Like
# the other macros, `derive` must be explicitly imported; the call
# site is the standard unresolved-name error (exit 13).
set -u
cd workdir || exit
"$KIO_BIN" check
