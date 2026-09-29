#!/bin/sh
# SUBJECT: Rust renders sealed removed host functions with their specified deprecation and default bodies.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
rm -rf "$scratch/out"
mkdir -p "$scratch/cargo-target" "$scratch/rust-tmp"

cd "$scratch"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
host_rs=$(cat out/rust/src/host.rs)

printf '%s' "$host_rs" | grep -q "type api__Str: Clone + PartialEq + 'static + From<String>;" || {
  printf 'missing exact live Str associated type\n' >&2
  exit 1
}
printf '%s' "$host_rs" | grep -q 'fn api__open(&self) -> Self::api__Str;' || {
  printf 'missing live open requirement\n' >&2
  exit 1
}
# shellcheck disable=SC2016
printf '%s' "$host_rs" \
  | grep -qF '#[deprecated(note = "host fn `api__log` removed at v(2)")]' || {
  printf 'missing sealed removal deprecation\n' >&2
  exit 1
}
printf '%s' "$host_rs" \
  | grep -Eq 'fn api__log\(&self, [A-Za-z_][A-Za-z0-9_]*: Self::api__Str\) \{' || {
  printf 'missing frozen removed method signature\n' >&2
  exit 1
}
# shellcheck disable=SC2016
printf '%s' "$host_rs" \
  | grep -qF 'unimplemented!("host fn `api__log` removed at v(2)")' || {
  printf 'missing removed method default body\n' >&2
  exit 1
}
(
  cd out/rust
  CARGO_TARGET_DIR="$scratch/cargo-target" \
    TMPDIR="$scratch/rust-tmp" \
    cargo check --offline --quiet
)
