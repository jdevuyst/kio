#!/bin/sh
# Kio' is a strict syntactic subset of Kio, so neither compiler admits an
# artifact-only visibility bypass. Both invocations must reject the same source.
set -u

kio_prime="$(dirname "$KIO_BIN")/kio-prime"
cd workdir || exit

"$KIO_BIN" check
full_status=$?
"$kio_prime" check
prime_status=$?

if [ "$full_status" -ne 11 ] || [ "$prime_status" -ne 11 ]; then
  exit 1
fi
exit 11
