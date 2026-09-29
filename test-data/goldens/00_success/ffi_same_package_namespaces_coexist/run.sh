#!/bin/sh
# One source package, built twice for the same backend under two configured
# artifact namespaces. A single ordinary host loads both outputs through the
# coexist protocol, proving that distribution configuration—not source package
# identity—isolates every facade, symbol, runtime helper, and structural mint.
set -u

scratch=$(mktemp -d) || {
  printf 'cannot make scratch dir\n' >&2
  exit 1
}
trap 'rm -rf "$scratch"' EXIT

# The manifests deliberately differ in namespace configuration, while the Kio
# module trees must remain the same source package.
cmp workdir/ns_a/greeter.kio workdir/ns_b/greeter.kio >/dev/null || {
  printf 'same-package namespace fixture: root modules differ\n' >&2
  exit 1
}
cmp workdir/ns_a/greeter/main.kio workdir/ns_b/greeter/main.kio >/dev/null || {
  printf 'same-package namespace fixture: entry modules differ\n' >&2
  exit 1
}

cp -R workdir/ns_a workdir/ns_b "$scratch/" || exit 1
for config in ns_a ns_b; do
  (
    cd "$scratch/$config" || exit 1
    "$KIO_BIN" test >/dev/null || exit
    "$KIO_BIN" build "$KIO_TARGET" || exit
  ) || exit 1
done

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
  "$scratch/ns_a/out/$KIO_TARGET" \
  "$scratch/ns_b/out/$KIO_TARGET"
