#!/bin/sh
# The `kio-prime` binary is the kio-rs sibling of `kio` — same crate,
# different bin — so it lives next to `$KIO_BIN`. We invoke it
# directly to assert that ordinary `pure fn` declarations survive in
# Kio', independently of which kio impl drives the case.
#
# Other impls' kio-prime equivalents (when they exist) would be
# named similarly under each impl's tree, but for the kio-rs
# corpus we test the kio-rs `kio-prime` binary.
set -u
cd workdir || exit
kio_prime="$(dirname "$KIO_BIN")/kio-prime"
"$kio_prime" check
