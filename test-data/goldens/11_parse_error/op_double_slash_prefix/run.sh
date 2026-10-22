#!/bin/sh
# Separate slash words test word-slot alternation rather than a line comment.
set -u
cd workdir || exit
"$KIO_BIN" check
