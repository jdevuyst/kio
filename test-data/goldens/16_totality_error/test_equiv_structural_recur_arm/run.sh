#!/bin/sh
set -u
cd workdir || exit

# A Totality outcome is not an ordinary equiv-cache result. The second
# discharge must execute and fail again rather than reuse a false success.
"$KIO_BIN" test >/dev/null 2>&1
first_status=$?
if [ "$first_status" -ne 16 ]; then
  echo "first kio test exited $first_status instead of 16" >&2
  exit 1
fi
"$KIO_BIN" test
