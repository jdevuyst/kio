#!/bin/sh
# Dot-led one-dot op tokens are reserved. A bare `.` is held back for
# member access, lambdas, placeholder lambdas, and future dot-led
# syntax; dot-leading operator tokens must contain at least two dots
# (`..`, `.+.`). Non-leading dot runs such as `+.` and `<.>` are also
# admitted.
set -u
cd workdir || exit
"$KIO_BIN" check
