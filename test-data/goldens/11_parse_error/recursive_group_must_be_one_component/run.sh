#!/bin/sh
# A written type-recursive group cannot absorb an unrelated acyclic member.
set -u
cd workdir || exit
"$KIO_BIN" check
