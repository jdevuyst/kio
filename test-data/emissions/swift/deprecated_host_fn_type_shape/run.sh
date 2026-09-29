#!/bin/sh
# SUBJECT: Swift marks retained protocol history and aliases deprecated while omitting live adapter dispatch.
# CONTRACT: specs/backends/swift.md § Deprecated host items.
# Literal backticks below are part of the asserted generated diagnostics.
# shellcheck disable=SC2016
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.swift-deprecated-shape.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
rm -rf "$scratch/workdir/out"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

out=out/swift

grep -F '@available(*, deprecated, message: "Kio host type `api.Retired` is retained only for removed host declarations from contract v2")' \
  "$out/host.swift" >/dev/null
grep -F 'associatedtype api__Retired: ExpressibleByStringLiteral = Swift.String' \
  "$out/host.swift" >/dev/null
test "$(grep -F -c '@available(*, deprecated, message: "Kio host fn `api.archived` was removed at contract v2")' \
  "$out/host.swift")" -eq 2
grep -F 'func api__archived(_ arg0: Self.api__Retired) -> Self.api__Retired' \
  "$out/host.swift" >/dev/null
grep -F 'fatalError("Kio host fn `api.archived` was removed at contract v2")' \
  "$out/host.swift" >/dev/null
grep -F '@available(*, deprecated, message: "Kio host fn `api.archived` was removed at contract v2")' \
  "$out/ffi.swift" >/dev/null

if grep -F 'api__archived' "$out/pkg.swift" >/dev/null; then
  printf 'deprecated_host_fn_type_shape: removed host fn entered the live package adapter\n' >&2
  exit 1
fi
