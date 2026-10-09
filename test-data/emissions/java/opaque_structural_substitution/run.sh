#!/bin/sh
# SUBJECT: Generic host unboxing supplies both fields of a public product to Kio consumers.
# CONTRACT: specs/backends/java.md, Host record contract and Structural and nominal types.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-opaque-substitution.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
mkdir -p "$scratch/classes" "$scratch/java-tmp"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:unchecked \
  -d "$scratch/classes" out/java/opaque_substitution/*.java
javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:unchecked \
  -cp "$scratch/classes" -d "$scratch/classes" "$scratch/host/Driver.java"
java -Djava.io.tmpdir="$scratch/java-tmp" -cp "$scratch/classes" Driver
