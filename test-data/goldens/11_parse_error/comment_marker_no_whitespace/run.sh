#!/bin/sh
# A comment marker (`//` or `///`) must be followed immediately by
# whitespace, end-of-line, or end-of-file. A non-whitespace character
# flush against the marker — here `//note` — is a lex error, with no
# exception for alphanumeric / underscore characters. The compiler
# rejects this with exit code 11 (parse error).
set -u
cd workdir || exit
"$KIO_BIN" check
