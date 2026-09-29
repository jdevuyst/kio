#!/bin/sh
# A Markdown reference-link definition [name]: url overrides
# auto-resolution for that key. Even if `unknown_name` is not in
# scope, defining `[unknown_name]: url` suppresses the error.
set -u
"$KIO_BIN" doc check
