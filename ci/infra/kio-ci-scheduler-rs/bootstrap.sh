#!/bin/sh
#
# Resolve the immutable, Git-common cached kio-ci-scheduler binary.
# POSIX sh only.

set -eu

PROG=$0
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd -P)

is_windows_shell() {
  case "${OS:-}:${MSYSTEM:-}" in
    Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*) return 0 ;;
    *) return 1 ;;
  esac
}

GIT_COMMON_DIR=$(git -C "$REPO_ROOT" rev-parse \
  --path-format=absolute --git-common-dir 2>/dev/null) || {
  printf 'error: %s cannot discover the repository Git common directory\n' "$PROG" >&2
  exit 2
}
case "$GIT_COMMON_DIR" in
  /*|[A-Za-z]:[\\/]*) ;;
  *)
    printf 'error: %s received a non-absolute Git common directory: %s\n' \
      "$PROG" "$GIT_COMMON_DIR" >&2
    exit 2
    ;;
esac
if is_windows_shell; then
  command -v cygpath >/dev/null 2>&1 || {
    printf 'error: %s requires cygpath to bridge Git and Cargo paths on Windows\n' \
      "$PROG" >&2
    exit 2
  }
  GIT_COMMON_DIR=$(cygpath -u "$GIT_COMMON_DIR") || exit 2
fi

RUSTC_BIN=${RUSTC:-rustc}
rustc_version() (
  cd "$SCRIPT_DIR"
  "$RUSTC_BIN" -vV
)
RUSTC_VERSION=$(rustc_version) || {
  printf 'error: %s cannot query the Rust toolchain\n' "$PROG" >&2
  exit 2
}

resolve_build_command() (
  cd "$SCRIPT_DIR" || exit 1
  rbc_command=$(command -v "$1" 2>/dev/null) || exit 1
  case "$rbc_command" in
    /*|[A-Za-z]:[\\/]*) printf '%s\n' "$rbc_command" ;;
    *)
      rbc_dir=$(CDPATH='' cd -- "${rbc_command%/*}" 2>/dev/null && pwd -P) ||
        exit 1
      printf '%s/%s\n' "${rbc_dir%/}" "${rbc_command##*/}"
      ;;
  esac
)

CARGO_BIN=$(resolve_build_command cargo) || {
  printf 'error: %s cannot resolve Cargo\n' "$PROG" >&2
  exit 2
}
[ -f "$CARGO_BIN" ] || {
  printf 'error: %s Cargo command is not a file: %s\n' "$PROG" "$CARGO_BIN" >&2
  exit 2
}

# rustup exposes Cargo through the same shim as rustc. Hash the selected
# toolchain's real Cargo executable rather than only the stable shim.
cargo_identity_record() {
  cir_cargo=$(resolve_build_command cargo) || return 1
  [ -f "$cir_cargo" ] || return 1
  cir_identity=$cir_cargo
  if cir_rustup=$(resolve_build_command rustup 2>/dev/null) &&
     cmp -s "$cir_cargo" "$cir_rustup" 2>/dev/null; then
    cir_rustup_cargo=$(
      cd "$SCRIPT_DIR" || exit 1
      "$cir_rustup" which cargo 2>/dev/null || true
    )
    if [ -n "$cir_rustup_cargo" ] && [ -f "$cir_rustup_cargo" ]; then
      cir_identity=$cir_rustup_cargo
    fi
  fi
  cir_hash=$(git -C "$REPO_ROOT" hash-object "$cir_identity") || return $?
  printf '%s\t%s\t%s\n' "$cir_cargo" "$cir_identity" "$cir_hash"
}
CARGO_IDENTITY=$(cargo_identity_record) || exit $?
HOST=$(printf '%s\n' "$RUSTC_VERSION" | sed -n 's/^host: //p')
[ -n "$HOST" ] || {
  printf 'error: %s could not determine the Rust host target\n' "$PROG" >&2
  exit 2
}
case "$HOST" in
  *-windows-*) EXE=.exe ;;
  *) EXE= ;;
esac

source_records() {
  for source in "$SCRIPT_DIR/Cargo.toml" "$SCRIPT_DIR/Cargo.lock"; do
    hash=$(git -C "$REPO_ROOT" hash-object "$source") || exit $?
    printf '%s\t%s\n' "${source#"$SCRIPT_DIR/"}" "$hash"
  done
  find "$SCRIPT_DIR/src" -type f -name '*.rs' -print |
    LC_ALL=C sort |
    while IFS= read -r source; do
      hash=$(git -C "$REPO_ROOT" hash-object "$source") || exit $?
      printf '%s\t%s\n' "${source#"$SCRIPT_DIR/"}" "$hash"
    done
}
SOURCE_RECORDS=$(source_records) || exit $?

# Cargo merges configuration from the crate directory through its ancestors,
# plus CARGO_HOME. Hash every applicable file; paths join the record because
# merge precedence is location-sensitive.
config_records() (
  config_dir=$SCRIPT_DIR
  while :; do
    for config in "$config_dir/.cargo/config.toml" "$config_dir/.cargo/config"; do
      [ -f "$config" ] || continue
      hash=$(git -C "$REPO_ROOT" hash-object "$config") || exit $?
      printf '%s\t%s\n' "$config" "$hash"
    done
    [ "$config_dir" != / ] || break
    config_dir=${config_dir%/*}
    [ -n "$config_dir" ] || config_dir=/
  done
  cargo_home=${CARGO_HOME:-${HOME:-}/.cargo}
  # Cargo is invoked from the scheduler crate. Resolve a relative CARGO_HOME
  # from that same directory so key discovery and the build see one config.
  if is_windows_shell; then
    case "$cargo_home" in
      /*) ;;
      [A-Za-z]:[\\/]*|\\\\*)
        cargo_home=$(cygpath -u "$cargo_home") || exit 1
        ;;
      *)
        cargo_home=$(cd "$SCRIPT_DIR" && cygpath -au "$cargo_home") || exit 1
        ;;
    esac
  else
    case "$cargo_home" in
      /*) ;;
      *) cargo_home=$SCRIPT_DIR/$cargo_home ;;
    esac
  fi
  for config in "$cargo_home/config.toml" "$cargo_home/config"; do
    [ -f "$config" ] || continue
    hash=$(git -C "$REPO_ROOT" hash-object "$config") || exit $?
    printf '%s\t%s\n' "$config" "$hash"
  done
)
CONFIG_RECORDS=$(config_records) || exit $?

# Output-affecting build settings join the key. The target directory affects
# only placement and is excluded; arbitrary configured compiler wrappers are
# retained conservatively because they can transform the compiler invocation.
BUILD_FLAGS=$(
  env | LC_ALL=C sort | sed -n \
    -e '/^RUSTFLAGS=/p' \
    -e '/^RUSTC_WRAPPER=/p' \
    -e '/^RUSTC_WORKSPACE_WRAPPER=/p' \
    -e '/^CARGO_ENCODED_RUSTFLAGS=/p' \
    -e '/^CARGO_BUILD_/p' \
    -e '/^CARGO_PROFILE_/p' \
    -e '/^CARGO_INCREMENTAL=/p' \
    -e '/^CARGO_TARGET_DIR=/d' \
    -e '/^CARGO_TARGET_/p' \
    -e '/^SOURCE_DATE_EPOCH=/p'
)

# A wrapper is an executable part of the compiler invocation, not merely a
# cache location. Preserve the exact setting above, then also key the resolved
# file so changing PATH or replacing a wrapper in place cannot reuse a binary
# produced by different wrapper code. Cargo resolves relative wrapper paths
# from this crate directory, which is also where the private build runs.
resolve_wrapper_path() (
  rwp_value=$1
  cd "$SCRIPT_DIR" || exit 1
  if is_windows_shell; then
    case "$rwp_value" in
      [A-Za-z]:[\\/]*|\\\\*|*\\*)
        rwp_value=$(cygpath -u "$rwp_value") || exit 1
        ;;
      *) ;;
    esac
  fi
  case "$rwp_value" in
    */*) rwp_candidate=$rwp_value ;;
    *) rwp_candidate=$(command -v "$rwp_value" 2>/dev/null) || exit 1 ;;
  esac
  [ -f "$rwp_candidate" ] || exit 1
  case "$rwp_candidate" in
    */*)
      rwp_dir=${rwp_candidate%/*}
      rwp_name=${rwp_candidate##*/}
      ;;
    *)
      rwp_dir=.
      rwp_name=$rwp_candidate
      ;;
  esac
  rwp_dir=$(CDPATH='' cd -- "$rwp_dir" 2>/dev/null && pwd -P) || exit 1
  printf '%s/%s\n' "${rwp_dir%/}" "$rwp_name"
)

wrapper_identity_record() {
  wir_name=$1
  wir_value=$2
  [ -n "$wir_value" ] || return 0
  if wir_path=$(resolve_wrapper_path "$wir_value"); then
    wir_hash=$(git -C "$REPO_ROOT" hash-object "$wir_path") || return $?
    printf '%s\t%s\t%s\n' "$wir_name" "$wir_path" "$wir_hash"
  else
    # An unresolved setting cannot build a cold binary, but it must still have
    # a distinct key rather than reusing a previously resolved wrapper's output.
    printf '%s\tunresolved\n' "$wir_name"
  fi
}

wrapper_identity_records() {
  wrapper_identity_record RUSTC_WRAPPER "${RUSTC_WRAPPER:-}"
  wrapper_identity_record RUSTC_WORKSPACE_WRAPPER \
    "${RUSTC_WORKSPACE_WRAPPER:-}"
  wrapper_identity_record CARGO_BUILD_RUSTC_WRAPPER \
    "${CARGO_BUILD_RUSTC_WRAPPER:-}"
  wrapper_identity_record CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
    "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}"
}
WRAPPER_IDENTITIES=$(wrapper_identity_records) || exit $?

KEY=$(
  {
    printf '%s\n' 'kio-ci-scheduler-bootstrap-v3'
    printf 'rustc=%s\n' "$RUSTC_BIN"
    printf '%s\n' "$RUSTC_VERSION"
    printf 'cargo=%s\n' "$CARGO_IDENTITY"
    printf '%s\n' "$BUILD_FLAGS"
    printf '%s\n' "$WRAPPER_IDENTITIES"
    printf '%s\n' "$CONFIG_RECORDS"
    printf '%s\n' "$SOURCE_RECORDS"
    printf '%s\n' 'profile=dev' 'features=default' "target=$HOST"
  } | git -C "$REPO_ROOT" hash-object --stdin
) || exit $?

CACHE_ROOT=$GIT_COMMON_DIR/kio-ci-scheduler
BIN_DIR=$CACHE_ROOT/bin/$KEY
BIN=$BIN_DIR/kio-ci-scheduler$EXE

validate_binary() {
  vb_actual=$("$BIN" --build-id 2>/dev/null) || {
    printf 'error: cached kio-ci-scheduler cannot report its build ID: %s\n' "$BIN" >&2
    return 1
  }
  if [ "$vb_actual" != "$KEY" ]; then
    printf 'error: cached kio-ci-scheduler has build ID %s, expected %s: %s\n' \
      "$vb_actual" "$KEY" "$BIN" >&2
    return 1
  fi
}

if [ -f "$BIN" ] && [ -x "$BIN" ]; then
  validate_binary || exit $?
  printf '%s\n' "$BIN"
  exit 0
fi
if [ -e "$BIN" ]; then
  printf 'error: cached kio-ci-scheduler is not an executable file: %s\n' "$BIN" >&2
  exit 2
fi

TARGET_DIR=$CACHE_ROOT/target/$KEY
mkdir -p "$TARGET_DIR" "$BIN_DIR" || exit 1
NATIVE_TARGET_DIR=$TARGET_DIR
if is_windows_shell; then
  NATIVE_TARGET_DIR=$(cygpath -m "$TARGET_DIR") || exit 2
fi
(
  cd "$SCRIPT_DIR"
  CARGO_TARGET_DIR=$NATIVE_TARGET_DIR \
    KIO_CI_SCHEDULER_BUILD_ID=$KEY \
    sh "$REPO_ROOT/ci/cargo.sh" --bootstrap-kio-ci-scheduler \
      build --locked --quiet --bin kio-ci-scheduler --target "$HOST"
) || exit $?

# An editor or tool update may change a mutable file-backed key input while the
# short cold build is in flight. Never publish that mixed-generation binary
# under the earlier key; restart resolution so the next pass hashes and builds
# one coherent snapshot.
if [ "$(source_records)" != "$SOURCE_RECORDS" ] ||
   [ "$(config_records)" != "$CONFIG_RECORDS" ] ||
   [ "$(wrapper_identity_records)" != "$WRAPPER_IDENTITIES" ] ||
   [ "$(rustc_version)" != "$RUSTC_VERSION" ] ||
   [ "$(cargo_identity_record)" != "$CARGO_IDENTITY" ]; then
  exec sh "$SCRIPT_DIR/bootstrap.sh"
fi

BUILT=$TARGET_DIR/$HOST/debug/kio-ci-scheduler$EXE
if [ ! -f "$BUILT" ] || [ ! -x "$BUILT" ]; then
  printf 'error: kio-ci-scheduler build did not produce %s\n' "$BUILT" >&2
  exit 1
fi

TEMP_BIN=$BIN_DIR/.kio-ci-scheduler.$$
trap 'rm -f "$TEMP_BIN"' EXIT INT TERM HUP
cp "$BUILT" "$TEMP_BIN" || exit 1
chmod +x "$TEMP_BIN" || exit 1
TEMP_BUILD_ID=$("$TEMP_BIN" --build-id 2>/dev/null) || {
  printf 'error: built kio-ci-scheduler cannot report its build ID\n' >&2
  exit 1
}
[ "$TEMP_BUILD_ID" = "$KEY" ] || {
  printf 'error: built kio-ci-scheduler has build ID %s, expected %s\n' \
    "$TEMP_BUILD_ID" "$KEY" >&2
  exit 1
}

# A hard link publishes the complete executable without replacing an existing
# immutable entry. Concurrent cold resolvers share Cargo's target lock; the
# loser of this publication race reuses the winner's identical keyed binary.
if ! ln "$TEMP_BIN" "$BIN" 2>/dev/null; then
  if [ ! -f "$BIN" ] || [ ! -x "$BIN" ]; then
    printf 'error: cannot publish kio-ci-scheduler at %s\n' "$BIN" >&2
    exit 1
  fi
fi
rm -f "$TEMP_BIN"
trap - EXIT INT TERM HUP
validate_binary || exit $?
printf '%s\n' "$BIN"
