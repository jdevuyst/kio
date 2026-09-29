# shellcheck shell=sh
#
# Shared lifecycle guard for explicitly configured sccache wrappers.
# Entry points use it both for an early fail-fast probe and immediately before
# an admitted compiler command, keeping the compiler lease out of the latter
# probe process. This adapter is the sole executable-name recognizer. It
# accepts POSIX and Windows path separators plus the native `.exe` suffix.

kio_set_tool_name() {
  kstn_name=$1
  kstn_name=${kstn_name##*/}
  kstn_name=${kstn_name##*\\}
  case "$kstn_name" in
    *.[eE][xX][eE]) kstn_name=${kstn_name%????} ;;
    *) ;;
  esac
  # Native Windows command paths are case-insensitive. Canonicalize only the
  # small explicit tool vocabulary this adapter classifies; arbitrary command
  # names remain byte-preserving.
  case "$kstn_name" in
    [sS][cC][cC][aA][cC][hH][eE]) kstn_name=sccache ;;
    [cC][aA][rR][gG][oO]) kstn_name=cargo ;;
    [rR][uU][sS][tT][cC]) kstn_name=rustc ;;
    [gG][oO]) kstn_name=go ;;
    [jJ][aA][vV][aA][cC]) kstn_name=javac ;;
    [sS][wW][iI][fF][tT][cC]) kstn_name=swiftc ;;
    [gG][hH][cC]) kstn_name=ghc ;;
    *) ;;
  esac
}

kio_is_sccache_tool() {
  kio_set_tool_name "$1"
  [ "$kstn_name" = sccache ]
}

kio_any_sccache_wrapper() {
  for kasw_wrapper in "$@"; do
    [ -n "$kasw_wrapper" ] || continue
    if kio_is_sccache_tool "$kasw_wrapper"; then
      return 0
    fi
  done
  return 1
}

# A runner debug observer is an opaque outer executable, not a compiler or
# cache wrapper. Readiness therefore classifies the former command immediately
# inside that exact configured layer. Compare the complete configured value
# before any basename normalization: an observer is allowed to have a name such
# as `rustc`, and that spelling must not suppress readiness for an inner
# `sccache`. `ksicc_program` is the exact program path to classify or invoke.
kio_select_inner_compiler_command() {
  ksicc_program=$1
  if [ "$#" -gt 1 ] &&
     [ -n "${KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER+x}" ] &&
     [ -n "$KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER" ] &&
     [ "$1" = "$KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER" ]; then
    ksicc_program=$2
  fi
}

kio_sccache_readiness_required_for_command() {
  kio_select_inner_compiler_command "$@"
  kio_set_tool_name "$ksicc_program"
  ksrrfc_tool=$kstn_name
  case "$ksrrfc_tool" in
    cargo)
      kio_any_sccache_wrapper \
        "${RUSTC_WRAPPER:-}" \
        "${RUSTC_WORKSPACE_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}"
      ;;
    sccache) return 0 ;;
    rustc|go|javac|swiftc|ghc) return 1 ;;
    *)
      kio_any_sccache_wrapper \
        "${RUSTC_WRAPPER:-}" \
        "${RUSTC_WORKSPACE_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" \
        "${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}"
      ;;
  esac
}

kio_configure_sccache_wrappers() {
  for kcsw_wrapper in "$@"; do
    [ -n "$kcsw_wrapper" ] || continue
    if kio_is_sccache_tool "$kcsw_wrapper"; then
      SCCACHE_IDLE_TIMEOUT=0
      export SCCACHE_IDLE_TIMEOUT
    fi
  done
  return 0
}

kio_ensure_sccache_wrappers_ready() {
  kio_configure_sccache_wrappers "$@"
  for kesr_wrapper in "$@"; do
    [ -n "$kesr_wrapper" ] || continue
    if kio_is_sccache_tool "$kesr_wrapper"; then
      if ! KIO_CI_SCHEDULE_HELD='' \
        "$kesr_wrapper" --dist-status >/dev/null 2>&1; then
        printf 'error: could not contact configured sccache wrapper %s\n' \
          "$kesr_wrapper" >&2
        return 2
      fi
    fi
  done
  return 0
}

kio_configure_sccache_environment() {
  kio_configure_sccache_wrappers \
    "${RUSTC_WRAPPER:-}" \
    "${RUSTC_WORKSPACE_WRAPPER:-}" \
    "${CARGO_BUILD_RUSTC_WRAPPER:-}" \
    "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" \
    "${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}"
}

kio_ensure_sccache_ready() {
  kio_ensure_sccache_wrappers_ready \
    "${RUSTC_WRAPPER:-}" \
    "${RUSTC_WORKSPACE_WRAPPER:-}" \
    "${CARGO_BUILD_RUSTC_WRAPPER:-}" \
    "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" \
    "${KIO_TEST_RUNNER_COMPILER_WRAPPER:-}"
}

kio_configure_sccache_for_command() {
  kio_select_inner_compiler_command "$@"
  kio_set_tool_name "$ksicc_program"
  kesfc_tool=$kstn_name
  case "$kesfc_tool" in
    cargo)
      kio_configure_sccache_wrappers \
        "${RUSTC_WRAPPER:-}" \
        "${RUSTC_WORKSPACE_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}"
      ;;
    sccache)
      SCCACHE_IDLE_TIMEOUT=0
      export SCCACHE_IDLE_TIMEOUT
      ;;
    rustc|go|javac|swiftc|ghc) ;;
    *) kio_configure_sccache_environment ;;
  esac
}

kio_ensure_sccache_ready_for_command() {
  kio_select_inner_compiler_command "$@"
  kio_set_tool_name "$ksicc_program"
  kesrfc_tool=$kstn_name
  case "$kesrfc_tool" in
    cargo)
      kio_ensure_sccache_wrappers_ready \
        "${RUSTC_WRAPPER:-}" \
        "${RUSTC_WORKSPACE_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WRAPPER:-}" \
        "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}"
      ;;
    sccache)
      SCCACHE_IDLE_TIMEOUT=0
      export SCCACHE_IDLE_TIMEOUT
      if ! KIO_CI_SCHEDULE_HELD='' \
        "$ksicc_program" --dist-status >/dev/null 2>&1; then
        printf 'error: could not contact configured sccache wrapper %s\n' \
          "$ksicc_program" >&2
        return 2
      fi
      ;;
    rustc|go|javac|swiftc|ghc) ;;
    *) kio_ensure_sccache_ready ;;
  esac
}
