#!/bin/sh
# Doc-comment {@} snippets: a `kio` fence inside a `///` doc-comment
# with `{@}` wraps the snippet in a synthetic fn in the surrounding
# module's namespace. Both module-level and per-item doc-comments are
# validated. Private items are visible inside {@} snippets.
set -u
"$KIO_BIN" doc check
