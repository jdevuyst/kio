#!/bin/sh
# SUBJECT: Java compiles unchanged and live-only hosts while retained defaults stay outside live dispatch.
set -eu

case_dir=$(pwd)
scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
cp -R "$case_dir/host" "$scratch/host"
rm -rf "$scratch/out"
mkdir -p "$scratch/classes" "$scratch/java-tmp"

cd "$scratch"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
TMPDIR="$scratch/java-tmp" \
  javac -J-Djava.io.tmpdir="$scratch/java-tmp" \
    -d "$scratch/classes" \
    out/java/app/*.java \
    "$scratch/host/RetainedHost.java"
TMPDIR="$scratch/java-tmp" \
  java -Djava.io.tmpdir="$scratch/java-tmp" \
    -cp "$scratch/classes" \
    RetainedHost
