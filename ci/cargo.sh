#!/bin/sh
#
# Top-level repository Cargo entry point. Runs `cargo` in the caller's current
# directory with the given arguments, without adding compiler-cache
# configuration. The wrapper admits commands to the shared compiler resource
# and optional Git-common cargo resource while preserving Cargo's complete CLI.
#
# POSIX sh only.

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/infra/sccache.sh"

SCHEDULER_BOOTSTRAP=
if [ "${1:-}" = --bootstrap-kio-ci-scheduler ]; then
  SCHEDULER_BOOTSTRAP=1
  shift
fi

held_has() {
  case ",${KIO_CI_SCHEDULE_HELD:-}," in
    *",$1,"*) return 0 ;;
    *) return 1 ;;
  esac
}

case "${KIO_CI_SERIALIZE_CARGO:-}" in
  ''|1) ;;
  *)
    printf 'ci/cargo.sh: KIO_CI_SERIALIZE_CARGO must be 1 or unset (got %s)\n' \
      "$KIO_CI_SERIALIZE_CARGO" >&2
    exit 2
    ;;
esac

if [ -n "$SCHEDULER_BOOTSTRAP" ]; then
  if [ -n "${KIO_CI_SCHEDULE_HELD:-}" ] ||
     [ -n "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ]; then
    printf 'ci/cargo.sh: cannot cold-bootstrap the scheduler under inherited admission\n' >&2
    exit 2
  fi
  # No native scheduler exists yet. This one direct probe is safe only because
  # the private bootstrap rejects every inherited scheduler context above.
  kio_configure_sccache_for_command cargo
  kio_ensure_sccache_ready_for_command cargo || exit $?

  scheduler_crate=$(CDPATH='' cd -- \
    "$SCRIPT_DIR/infra/kio-ci-scheduler-rs" && pwd -P) || exit 2
  scheduler_cwd=$(pwd -P) || exit 2
  if [ "$scheduler_cwd" != "$scheduler_crate" ]; then
    printf 'ci/cargo.sh: scheduler bootstrap must run from %s\n' \
      "$scheduler_crate" >&2
    exit 2
  fi

  rustc_bin=${RUSTC:-rustc}
  scheduler_host=$("$rustc_bin" -vV 2>/dev/null | sed -n 's/^host: //p')
  if [ -z "$scheduler_host" ]; then
    printf 'ci/cargo.sh: scheduler bootstrap cannot determine the Rust host target\n' >&2
    exit 2
  fi
  if [ "$#" -ne 7 ] ||
     [ "$1" != build ] || [ "$2" != --locked ] || [ "$3" != --quiet ] ||
     [ "$4" != --bin ] || [ "$5" != kio-ci-scheduler ] ||
     [ "$6" != --target ] || [ "$7" != "$scheduler_host" ]; then
    printf 'ci/cargo.sh: invalid private kio-ci-scheduler bootstrap command\n' >&2
    exit 2
  fi
  case "${CARGO_TARGET_DIR:-}" in
    /*|[A-Za-z]:[\\/]*) ;;
    *)
      printf 'ci/cargo.sh: scheduler bootstrap requires an absolute CARGO_TARGET_DIR\n' >&2
      exit 2
      ;;
  esac
  case "${KIO_CI_SCHEDULER_BUILD_ID:-}" in
    ''|*[!0-9a-f]*)
      printf 'ci/cargo.sh: scheduler bootstrap requires a hexadecimal build ID\n' >&2
      exit 2
      ;;
    *) ;;
  esac
  exec cargo "$@"
fi

if [ "${KIO_CI_SCHEDULE:-}" = DISABLE ]; then
  # Keep the explicit admission bypass while retaining the facade-owned
  # compiler-wrapper configuration and isolated daemon-readiness check.
  exec sh "$SCRIPT_DIR/schedule.sh" --resource compiler -- cargo "$@"
fi

if [ "${KIO_CI_SERIALIZE_CARGO:-}" = 1 ] && ! held_has cargo; then
  # Re-enter after cargo admission so the same wrapper then acquires compiler.
  # The native held-resource validator rejects any attempted order inversion.
  exec sh "$SCRIPT_DIR/schedule.sh" --resource cargo -- sh "$0" "$@"
fi

# A corpus worker retains inherited work. Standalone Cargo takes no work slot,
# so a queued build cannot block fresh corpus admission. Re-entering an already
# held compiler resource is validated and reused by the native scheduler.
exec sh "$SCRIPT_DIR/schedule.sh" --resource compiler -- cargo "$@"
