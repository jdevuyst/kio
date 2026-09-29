#!/bin/sh
# Fetch does not precheck private uses; ordinary checking enforces the terminal projector's visibility.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch --force >/dev/null || exit
printf 'fetch accepted\n'
"$KIO_BIN" check
