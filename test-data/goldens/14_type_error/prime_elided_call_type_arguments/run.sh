#!/bin/sh
# Surface Kio may infer the complete `[A] A -> A` type argument from the
# polymorphic value, while Kio' requires that same argument explicitly.
# Check the explicit Kio' control before exercising the omitted rejection.
set -u
cd workdir || exit
if [ "$(basename "$KIO_BIN")" = kio ]; then
  for fixture in explicit omitted; do
    if ! (cd "$fixture" && "$KIO_BIN" check) >/dev/null 2>&1; then
      printf 'surface kio unexpectedly rejected the %s polytype argument\n' "$fixture" >&2
      exit 1
    fi
  done
fi
kio_prime="$(dirname "$KIO_BIN")/kio-prime"
if ! (cd explicit && "$kio_prime" check) >/dev/null 2>&1; then
  printf 'kio-prime unexpectedly rejected the explicit polytype control\n' >&2
  exit 1
fi
(cd omitted && "$kio_prime" check)
