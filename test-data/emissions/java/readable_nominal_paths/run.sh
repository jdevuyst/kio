#!/bin/sh
# SUBJECT: Readable declaration-owned Java paths preserve indices, privacy, retained types, and additive naming at native classfile limits.
# CONTRACT: specs/backends/java.md, Item naming and Higher-kinded types.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-nominal-paths.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
mkdir -p "$scratch/classes" "$scratch/java-tmp"
cd "$scratch/workdir"

check_host() {
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:unchecked \
    -d "$scratch/classes" out/java/names/*.java
  javac -J-Djava.io.tmpdir="$scratch/java-tmp" -Xlint:all -Werror \
    -cp "$scratch/classes" -d "$scratch/classes" "$scratch/host/Driver.java"
  java -Djava.io.tmpdir="$scratch/java-tmp" -cp "$scratch/classes" Driver
  for negative in WrongIdentity WrongStorage HiddenPayload HiddenConstructor; do
    if javac -J-Djava.io.tmpdir="$scratch/java-tmp" -cp "$scratch/classes" \
      -d "$scratch/classes" "$scratch/host/$negative.java" >"$scratch/negative.log" 2>&1; then
      printf '%s compiled despite violating the public interface\n' "$negative" >&2
      exit 1
    fi
  done
}

check_host
printf '\npub newtype Unrelated : . { pub constructor make_unrelated; pub projector read_unrelated }\n' >> api.kio
check_host
