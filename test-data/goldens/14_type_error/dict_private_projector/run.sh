#!/bin/sh
# The dictionary's nominal projector is private so callers cannot expose node state.
set -u
cd workdir || exit
"$KIO_BIN" check
