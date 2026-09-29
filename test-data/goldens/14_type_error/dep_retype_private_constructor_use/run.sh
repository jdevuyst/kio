#!/bin/sh
# Fetch admits private member differences; ordinary checking diagnoses a retained missing constructor.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch --force >/dev/null || exit
printf 'fetch accepted\n'
"$KIO_BIN" check
