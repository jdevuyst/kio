#!/bin/sh
# The dictionary's nominal constructor is private so callers cannot forge payloads.
set -u
cd workdir || exit
"$KIO_BIN" check
