#!/bin/sh
# `import helper(op _ + _);` and the module-local `op _ + _` name distinct
# operator origins. All imports resolve before the collision is reported as a
# name error (exit 13).
set -u
cd workdir || exit
"$KIO_BIN" check
