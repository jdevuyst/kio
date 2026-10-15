#!/bin/sh
# Caller Cargo configuration survives selected Rust execution and nested mise.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)
work=$(mktemp -d)
cleanup() {
  if [ -e "$work/poisoned-used" ]; then printf 'poisoned compiler selected\n' >&2; fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
mkdir -p "$work/cargo" "$work/project/src"
printf '%s\n' '[package]' 'name = "toolchain-caller-witness"' 'version = "0.0.0"' 'edition = "2021"' >"$work/project/Cargo.toml"
printf '%s\n' 'fn main() { assert_eq!(env!("KIO_CALLER_CONFIG"), "preserved"); println!("42"); }' >"$work/project/src/main.rs"
# shellcheck disable=SC2016 # The poisoned executable writes its own marker.
printf '%s\n' '#!/bin/sh' ': >"$KIO_TEST_POISON_MARKER"' 'exit 97' >"$work/poisoned-rustc"
chmod +x "$work/poisoned-rustc"
printf '%s\n' '[build]' "rustc = \"$work/poisoned-rustc\"" '[env]' 'KIO_CALLER_CONFIG = "preserved"' >"$work/cargo/config.toml"
cat >"$work/wrapper" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$1" >>"$KIO_CALLER_WRAPPER_LOG"
if [ -n "${KIO_CALLER_ORIGINAL_WRAPPER:-}" ]; then exec "$KIO_CALLER_ORIGINAL_WRAPPER" "$@"; fi
exec "$@"
EOF
chmod +x "$work/wrapper"
cat >"$work/check" <<'EOF'
#!/bin/sh
set -eu
[ "${KIO_TEST_TRACE:-}" != 1 ] || set -x
[ "$CARGO_HOME" = "$KIO_EXPECTED_CARGO_HOME" ]
[ "$RUSTC_WRAPPER" = "$KIO_EXPECTED_WRAPPER" ]
cd "$KIO_CALLER_PROJECT"
before=$(rustc --print sysroot)
case "$before" in "$KIO_TOOLCHAINS_ROOT/rustup/toolchains/"*) ;; *) exit 1 ;; esac
cargo run --offline --quiet
if [ "${KIO_TEST_DIRECT_CARGO:-}" != 1 ]; then
  mise exec rust -- sh -c '[ "$CARGO_HOME" = "$KIO_EXPECTED_CARGO_HOME" ]; [ "$RUSTC_WRAPPER" = "$KIO_EXPECTED_WRAPPER" ]; cargo run --offline --quiet'
fi
[ "$(rustc --print sysroot)" = "$before" ]
[ -s "$KIO_CALLER_WRAPPER_LOG" ]
EOF
export KIO_CALLER_ORIGINAL_WRAPPER="${RUSTC_WRAPPER-}"
export CARGO_HOME="$work/cargo" RUSTC_WRAPPER="$work/wrapper"
export RUSTC="$work/poisoned-rustc" RUSTDOC="$work/poisoned-rustc"
export KIO_TEST_POISON_MARKER="$work/poisoned-used"
export KIO_EXPECTED_CARGO_HOME="$CARGO_HOME" KIO_EXPECTED_WRAPPER="$RUSTC_WRAPPER"
export KIO_CALLER_PROJECT="$work/project" KIO_CALLER_WRAPPER_LOG="$work/wrapper.log"
sh "${KIO_TEST_TOOLCHAIN_OWNER:-$repo_root/ci/impl-toolchain.sh}" toolchain-exec rust -- sh "$work/check"
[ ! -e "$work/poisoned-used" ]
printf 'toolchain-cargo-selftest: caller config, wrapper, nested selection and compile/run passed\n'
