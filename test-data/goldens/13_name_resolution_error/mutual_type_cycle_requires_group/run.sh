#!/bin/sh
# Mutual nominal declarations require one explicit recursive type group.
set -u
cd workdir || exit
"$KIO_BIN" check
