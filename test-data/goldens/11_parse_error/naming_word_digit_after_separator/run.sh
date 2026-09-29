#!/bin/sh
# SUBJECT: Each underscore-separated source word begins with a letter.
set -u
cd workdir || exit
"$KIO_BIN" check
