#!/bin/sh
# SUBJECT: Typed host callbacks preserve compound payloads through opaque constructor applications.
# CONTRACT: specs/backends/java.md, Higher-kinded types and FFI surface.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-substitution.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
mkdir -p "$scratch/classes" "$scratch/java-tmp"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:unchecked \
  -d "$scratch/classes" out/java/substitution/*.java
javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:unchecked -Werror \
  -cp "$scratch/classes" -d "$scratch/classes" "$scratch/host/Substitution.java"
java -Djava.io.tmpdir="$scratch/java-tmp" -cp "$scratch/classes" Substitution
