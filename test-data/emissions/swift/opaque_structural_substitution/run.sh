#!/bin/sh
# SUBJECT: Opaque generic storage preserves native product, absent sum, and staged callable indices in both directions.
# CONTRACT: specs/backends/swift.md, Host record contract and Structural and nominal types.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.swift-opaque-substitution.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

out="$scratch/workdir/out/swift"
host_build="$scratch/host-build"
mkdir -p "$host_build/.module-cache"
swiftc \
  -Onone \
  -module-name OpaqueSubstitution \
  -module-cache-path "$host_build/.module-cache" \
  -emit-module \
  -emit-module-path "$host_build/OpaqueSubstitution.swiftmodule" \
  -emit-library \
  -static \
  -o "$host_build/libOpaqueSubstitution.a" \
  "$out"/*.swift
swiftc \
  -Onone \
  -module-cache-path "$host_build/.module-cache" \
  -I "$host_build" \
  -L "$host_build" \
  -lOpaqueSubstitution \
  "$scratch/host/driver.swift" \
  -o "$host_build/driver"
"$host_build/driver"
