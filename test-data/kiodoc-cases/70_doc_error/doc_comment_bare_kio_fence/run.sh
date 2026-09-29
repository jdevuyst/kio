#!/bin/sh
# A bare ```kio fence (no attribute list) inside a /// doc-comment
# is a runner error — authors must write {@} or {ignore}.
set -u
"$KIO_BIN" doc check
