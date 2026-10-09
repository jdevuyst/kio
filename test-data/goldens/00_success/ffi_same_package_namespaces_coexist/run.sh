#!/bin/sh
# One source package, two named targets for the same backend under two configured
# artifact namespaces. A single ordinary host loads both outputs through the
# coexist protocol, proving that distribution configuration—not source package
# identity—isolates every facade, symbol, runtime helper, and structural mint.
set -u

scratch=$(mktemp -d) || {
  printf 'cannot make scratch dir\n' >&2
  exit 1
}
trap 'rm -rf "$scratch"' EXIT

cp -R workdir/. "$scratch/" || exit 1
(
  cd "$scratch" || exit 1
  "$KIO_BIN" test >/dev/null || exit
  "$KIO_BIN" build "$KIO_TARGET-a" "$KIO_TARGET-b" || exit
  # Repeat against the same package cache before the two-artifact runtime
  # assertion, so cached output must retain each target's configuration.
  "$KIO_BIN" build "$KIO_TARGET-a" "$KIO_TARGET-b" || exit
) || exit 1

case "$KIO_TARGET" in
  swift)
    namespace_a=SharedA
    namespace_b=SharedB
    ;;
  haskell)
    namespace_a=Data.Text
    namespace_b=Data.Text.Runtime
    ;;
  *)
    namespace_a=shared_a
    namespace_b=shared_b
    ;;
esac

exec "$KIO_RUNNER" \
  --package-name shared_pkg \
  --artifact-namespace "$namespace_a" \
  --package-name shared_pkg \
  --artifact-namespace "$namespace_b" \
  --protocol coexist \
  "$scratch/out/$KIO_TARGET-a" \
  "$scratch/out/$KIO_TARGET-b"
