#!/bin/sh
# Four or more slashes (`////`) are not a section ruler — the `///`
# doc-comment marker must be followed by whitespace, end-of-line, or
# end-of-file, and a fourth `/` is a non-whitespace character flush
# against the marker. The compiler rejects this with exit code 11
# (parse error), reserving the `//…` family for future syntax.
set -u
cd workdir || exit
"$KIO_BIN" check
