#!/bin/sh
# SUBJECT: Java source grouping distinguishes native import/export function types and preserves callback stages.
# CONTRACT: specs/backends/java.md, FFI surface.
set -eu

case_dir=$(pwd)
scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-source-groups.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R "$case_dir/host" "$scratch/host"
mkdir -p "$scratch/classes" "$scratch/java-tmp"
cd "$scratch/workdir"
"${KIO_BIN:?}" test >&2
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
TMPDIR="$scratch/java-tmp" \
  javac -J-Djava.io.tmpdir="$scratch/java-tmp" \
    -d "$scratch/classes" out/java/grouped/*.java "$scratch/host/SourceGroups.java"
TMPDIR="$scratch/java-tmp" \
  java -Djava.io.tmpdir="$scratch/java-tmp" -cp "$scratch/classes" SourceGroups
