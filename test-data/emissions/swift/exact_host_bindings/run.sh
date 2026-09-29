#!/bin/sh
# SUBJECT: The Swift facade accepts exact module-qualified host bindings and documented literal-role constraints.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.swift.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM

cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

out="$scratch/workdir/out/swift"
host_build="$scratch/host-build"
mkdir -p "$host_build/.module-cache"
swiftc \
  -Onone \
  -module-name FfiSwiftExactHostBindings \
  -module-cache-path "$host_build/.module-cache" \
  -emit-module \
  -emit-module-path "$host_build/FfiSwiftExactHostBindings.swiftmodule" \
  -emit-library \
  -static \
  -o "$host_build/libFfiSwiftExactHostBindings.a" \
  "$out"/*.swift
swiftc \
  -Onone \
  -module-cache-path "$host_build/.module-cache" \
  -I "$host_build" \
  -L "$host_build" \
  -lFfiSwiftExactHostBindings \
  "$scratch/host/driver.swift" \
  -o "$host_build/driver"
"$host_build/driver"
