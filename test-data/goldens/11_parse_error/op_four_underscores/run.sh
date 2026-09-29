#!/bin/sh
# Underscore runs of 4 or more are reserved; only `_`, `__`, and
# `___` are slot tokens in op patterns.
set -u
cd workdir || exit
"$KIO_BIN" check
