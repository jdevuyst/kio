#!/bin/sh
# A qualified callee's declared type keeps the callee module's identity; a
# caller-local alias with the same leaf cannot reinterpret that signature.
set -u
cd workdir || exit
"$KIO_BIN" check
