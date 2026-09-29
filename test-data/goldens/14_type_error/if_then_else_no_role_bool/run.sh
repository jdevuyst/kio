#!/bin/sh
# A qualified module import alone does not place its `role(bool)` member in the
# consumer's unqualified candidate pool, so the intrinsic has no scheme there.
set -u
cd workdir || exit
"$KIO_BIN" check
