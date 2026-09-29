#!/bin/sh
# Type-parameter binders must use an exact type-name spelling. This pins the
# parser-level spelling contract at a direct `[T]` binder site.
set -u
cd workdir || exit
"$KIO_BIN" check
