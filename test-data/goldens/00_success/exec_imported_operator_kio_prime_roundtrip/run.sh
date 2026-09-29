#!/bin/sh
# An imported operator lowers to an ordinary qualified call. Its emitted Kio'
# must contain the matching ordinary import, carry no visibility privilege, and
# pass a fresh Kio'-only check before the compiled package runs.
set -u

kio_prime="$(dirname "$KIO_BIN")/kio-prime"
cd workdir || exit

"$KIO_BIN" test >/dev/null || exit
"$KIO_BIN" build kio-prime >/dev/null || exit

prime_main=out/kio-prime/testapi/main.kio
provider_alias=$(
  awk '
    /^import ops as [a-z_][a-z0-9_]*;$/ {
      value = $4
      sub(/;$/, "", value)
      count++
    }
    END {
      if (count != 1) exit 1
      print value
    }
  ' "$prime_main"
) || exit

if grep -Fq '__internal__' "$prime_main"; then
  exit 1
fi
grep -Fq "$provider_alias.add" "$prime_main" || exit
(cd out/kio-prime && "$kio_prime" check) || exit

"$KIO_BIN" build "$KIO_TARGET" >/dev/null || exit
"$KIO_RUNNER" --package-name exec_imported_operator_kio_prime_roundtrip \
  --protocol testapi-print "out/$KIO_TARGET"
