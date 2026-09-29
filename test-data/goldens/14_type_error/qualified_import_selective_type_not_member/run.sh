#!/bin/sh
# A selective import is local scope, not a qualified module member.
set -u
cd workdir || exit
"$KIO_BIN" check
