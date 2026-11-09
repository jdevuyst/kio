#!/bin/sh
# SUBJECT: Boolean selection preserves exact host objects and their tags through inline operands, opaque callbacks, and eager thunk factories.
# CONTRACT: specs/backends/java.md § Host record contract; shared Boolean operand semantics.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-boolean-carrier.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
mkdir -p "$scratch/classes" "$scratch/java-tmp"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
TMPDIR="$scratch/java-tmp" \
  javac -J-Djava.io.tmpdir="$scratch/java-tmp" \
    -d "$scratch/classes" out/java/boolean_carrier/*.java "$scratch/host/Driver.java"
TMPDIR="$scratch/java-tmp" \
  java -Djava.io.tmpdir="$scratch/java-tmp" -cp "$scratch/classes" Driver
