#!/bin/sh
# The bare `@` self-ref wraps a snippet in its surrounding module, so it
# is a `///` doc-comment form. On a `.md` fence there is no surrounding
# module and `{@}` is a runner error — it must not silently degrade to a
# standalone snippet with the `@` ignored.
set -u
"$KIO_BIN" doc check
