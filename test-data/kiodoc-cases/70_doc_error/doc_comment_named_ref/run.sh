#!/bin/sh
# A fence with {@NAME} inside a /// doc-comment is a runner error.
# Inside doc-comments, the harness is always the surrounding module;
# use {@} instead of {@NAME}.
set -u
"$KIO_BIN" doc check
