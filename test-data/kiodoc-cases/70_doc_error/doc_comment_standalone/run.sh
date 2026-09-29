#!/bin/sh
# A fence with {} (standalone) inside a /// doc-comment is a runner
# error. Inside doc-comments only {@} and {ignore} are supported.
set -u
"$KIO_BIN" doc check
