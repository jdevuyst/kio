#!/bin/sh
# SUBJECT: History-only nominal namespace holders are deprecated while live descendants keep shared holders live.
# CONTRACT: specs/backends/java.md, Deprecated host items and Item naming.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-retained-holders.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
mkdir -p "$scratch/classes" "$scratch/java-tmp"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:unchecked \
  -d "$scratch/classes" out/java/retention/*.java
javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:all -Werror \
  -cp "$scratch/classes" -d "$scratch/classes" "$scratch/host/Driver.java"
java -Djava.io.tmpdir="$scratch/java-tmp" -cp "$scratch/classes" Driver
