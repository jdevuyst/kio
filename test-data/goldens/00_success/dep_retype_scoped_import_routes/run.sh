#!/bin/sh
# Equal scoped source and target visibility admits selective and qualified
# dependency consumers. Fetch and check exercise the shared frontend only.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch --force >/dev/null || exit
"$KIO_BIN" check >/dev/null || exit
printf 'scoped import routes preserved\n'
