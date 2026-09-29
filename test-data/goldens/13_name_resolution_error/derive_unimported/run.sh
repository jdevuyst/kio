#!/bin/sh
# `derive!` called with no `import <module>(derive);` declaration. Like
# the other elaborators, `derive!` must be explicitly imported; the call
# site is the standard unresolved-name error (exit 13), naming the
# import line to add.
set -u
cd workdir || exit
"$KIO_BIN" check
