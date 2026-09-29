#!/bin/sh
# Type declarations must use an exact type-name spelling. This pins the
# parser-level spelling contract at a direct type-definition site.
set -u
cd workdir || exit
"$KIO_BIN" check
