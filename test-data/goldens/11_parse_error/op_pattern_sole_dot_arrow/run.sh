#!/bin/sh
# Dot-led one-dot op tokens are reserved. `.>` is held back for UFCS
# dispatch and future dot-led syntax; dot-leading operator tokens must
# contain at least two dots (`..`, `.+.`).
set -u
cd workdir || exit
"$KIO_BIN" check
