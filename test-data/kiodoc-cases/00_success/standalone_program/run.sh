#!/bin/sh
# A standalone snippet — `{}` empty attribute list, body is a
# complete Kio package file plus module body per specs/kiodoc.md
# § Standalone snippet.
set -u
"$KIO_BIN" doc check
