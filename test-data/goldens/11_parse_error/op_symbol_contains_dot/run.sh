#!/bin/sh
# Dots retain their native structural roles and cannot form operator words.
set -u
cd workdir || exit
"$KIO_BIN" check
