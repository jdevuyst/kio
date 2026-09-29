#!/bin/sh
#
# Thin shell facade for the native Kio CI scheduler.
#
# POSIX sh only. Resource policy, locking, queueing, and process-tree ownership
# live in ci/infra/kio-ci-scheduler-rs.

set -u

# Capture the caller's descriptor topology before command substitutions can
# reuse a closed standard-input descriptor for their own work.
STDIN_CLOSED=
if ! ( exec 7<&0 ) 2>/dev/null; then
  STDIN_CLOSED=1
fi

PROG=$0
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)

usage() {
  cat <<EOF
Usage:
  sh $PROG [--barrier] -- <command> [args...]
  sh $PROG --resource cargo -- <command> [args...]
  sh $PROG --resource compiler -- <command> [args...]
  sh $PROG --prepare
  sh $PROG --self-test
  sh $PROG --available-parallelism
  sh $PROG --state-dir
  sh $PROG --supervise --drained-marker <path> --cancel-file <path> -- <command> [args...]
  sh $PROG --readiness -- <command> [args...]

Run one command through Kio's shared native scheduler. Work is the default
resource; --barrier changes only a newly acquired work claim. Cargo is the
optional capacity-one whole-Cargo resource. Compiler admission is fixed when
KIO_CI_SCHEDULE_COMPILER_JOBS is set and adaptive otherwise.

KIO_CI_SCHEDULE=DISABLE is the explicit admission bypass.
--supervise owns one process tree without acquiring a scheduler resource and
remains active when admission is bypassed. Creating its cancellation file
requests tree termination; its drained marker appears only after extinction.
--readiness probes configured daemon wrappers without running the command.
EOF
}

validate_scheduler_bin() {
  vsb_bin=$1
  case "$vsb_bin" in
    /*|[A-Za-z]:[\\/]*) ;;
    *)
      printf 'error: KIO_CI_SCHEDULER_BIN must be an absolute path (got %s)\n' \
        "$vsb_bin" >&2
      return 2
      ;;
  esac
  if [ ! -f "$vsb_bin" ] || [ ! -x "$vsb_bin" ]; then
    printf 'error: KIO_CI_SCHEDULER_BIN is not an executable file: %s\n' \
      "$vsb_bin" >&2
    return 2
  fi
}

resolve_scheduler_bin() {
  case "${KIO_CI_SCHEDULER_BIN:-}" in
    '') sh "$SCRIPT_DIR/infra/kio-ci-scheduler-rs/bootstrap.sh" ;;
    *)
      validate_scheduler_bin "$KIO_CI_SCHEDULER_BIN" || return $?
      printf '%s\n' "$KIO_CI_SCHEDULER_BIN"
      ;;
  esac
}

is_windows_shell() {
  case "${OS:-}:${MSYSTEM:-}" in
    Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*) return 0 ;;
    *) return 1 ;;
  esac
}

publish_schedule_root() {
  if [ -n "${KIO_CI_SCHEDULE_DIR:-}" ]; then
    psr_root=$KIO_CI_SCHEDULE_DIR
  else
    psr_common=$(git -C "$SCRIPT_DIR/.." rev-parse \
      --path-format=absolute --git-common-dir 2>/dev/null) || {
      printf 'error: %s cannot discover the repository Git common directory\n' \
        "$PROG" >&2
      return 2
    }
    psr_root=$psr_common/kio-ci-schedule
  fi

  if is_windows_shell; then
    command -v cygpath >/dev/null 2>&1 || {
      printf 'error: %s requires cygpath to bridge the shell/native scheduler path on Windows\n' \
        "$PROG" >&2
      return 2
    }
    psr_shell_root=$(cygpath -u "$psr_root") || return 2
    mkdir -p "$psr_shell_root" || return 1
    psr_shell_root=$(CDPATH='' cd -- "$psr_shell_root" && pwd -P) || return 2
    KIO_CI_SCHEDULE_DIR=$(cygpath -m "$psr_shell_root") || return 2
  else
    mkdir -p "$psr_root" || return 1
    KIO_CI_SCHEDULE_DIR=$(CDPATH='' cd -- "$psr_root" && pwd -P) || return 2
  fi
  export KIO_CI_SCHEDULE_DIR
}

case "${1:-}" in
  --supervise)
    shift
    if [ "${1:-}" != --drained-marker ] || [ -z "${2:-}" ] ||
       [ "${3:-}" != --cancel-file ] || [ -z "${4:-}" ]; then
      printf 'error: %s --supervise requires --drained-marker <path> --cancel-file <path>\n' \
        "$PROG" >&2
      exit 2
    fi
    supervise_drained_marker=$2
    supervise_cancel_file=$4
    shift 4
    if is_windows_shell; then
      command -v cygpath >/dev/null 2>&1 || {
        printf 'error: %s requires cygpath to bridge supervision paths on Windows\n' \
          "$PROG" >&2
        exit 2
      }
      supervise_drained_marker=$(cygpath -m "$supervise_drained_marker") || exit $?
      supervise_cancel_file=$(cygpath -m "$supervise_cancel_file") || exit $?
    fi
    KIO_CI_SCHEDULER_BIN=$(resolve_scheduler_bin) || exit $?
    export KIO_CI_SCHEDULER_BIN
    if [ -n "$STDIN_CLOSED" ]; then
      exec "$KIO_CI_SCHEDULER_BIN" supervise --stdin-closed \
        --drained-marker "$supervise_drained_marker" \
        --cancel-file "$supervise_cancel_file" "$@"
    fi
    exec "$KIO_CI_SCHEDULER_BIN" supervise \
      --drained-marker "$supervise_drained_marker" \
      --cancel-file "$supervise_cancel_file" "$@"
    ;;
  --prepare)
    [ "$#" -eq 1 ] || {
      printf 'error: %s --prepare accepts no other arguments\n' "$PROG" >&2
      exit 2
    }
    resolve_scheduler_bin
    exit $?
    ;;
  --self-test)
    [ "$#" -eq 1 ] || {
      printf 'error: %s --self-test accepts no other arguments\n' "$PROG" >&2
      exit 2
    }
    KIO_CI_SCHEDULER_BIN=$(resolve_scheduler_bin) || exit $?
    export KIO_CI_SCHEDULER_BIN
    exec "$KIO_CI_SCHEDULER_BIN" self-test
    ;;
  --available-parallelism)
    [ "$#" -eq 1 ] || {
      printf 'error: %s --available-parallelism accepts no other arguments\n' \
        "$PROG" >&2
      exit 2
    }
    KIO_CI_SCHEDULER_BIN=$(resolve_scheduler_bin) || exit $?
    export KIO_CI_SCHEDULER_BIN
    exec "$KIO_CI_SCHEDULER_BIN" available-parallelism
    ;;
  --state-dir)
    [ "$#" -eq 1 ] || {
      printf 'error: %s --state-dir accepts no other arguments\n' "$PROG" >&2
      exit 2
    }
    publish_schedule_root || exit $?
    printf '%s\n' "$KIO_CI_SCHEDULE_DIR"
    exit 0
    ;;
  --readiness)
    shift
    [ "${1:-}" = -- ] || {
      printf 'error: %s --readiness expects -- before the command\n' "$PROG" >&2
      exit 2
    }
    shift
    [ "$#" -gt 0 ] || {
      printf 'error: %s --readiness requires a command\n' "$PROG" >&2
      exit 2
    }
    # Avoid resolving or building the native utility when no configured tool
    # can start a daemon. The adapter owns this explicit tool classification.
    # shellcheck disable=SC1091
    . "$SCRIPT_DIR/infra/sccache.sh"
    kio_sccache_readiness_required_for_command "$@" || exit 0
    if [ "${KIO_CI_SCHEDULE:-}" = DISABLE ] &&
       [ -z "${KIO_CI_SCHEDULER_BIN:-}" ] &&
       [ -z "${KIO_CI_SCHEDULE_HELD:-}" ] &&
       [ -z "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ]; then
      exec sh "$SCRIPT_DIR/schedule.sh" --compiler-readiness -- "$@"
    fi
    KIO_CI_SCHEDULER_BIN=$(resolve_scheduler_bin) || exit $?
    export KIO_CI_SCHEDULER_BIN
    exec "$KIO_CI_SCHEDULER_BIN" readiness -- \
      sh "$SCRIPT_DIR/schedule.sh" --compiler-readiness -- "$@"
    ;;
  --compiler-readiness)
    shift
    [ "${1:-}" = -- ] || {
      printf 'error: invalid internal compiler-readiness invocation\n' >&2
      exit 2
    }
    shift
    [ "$#" -gt 0 ] || {
      printf 'error: internal compiler readiness requires a command\n' >&2
      exit 2
    }
    # This is the explicit tooling adapter. The native scheduler knows only
    # the generic hook protocol and never matches sccache or another tool name.
    # shellcheck disable=SC1091
    . "$SCRIPT_DIR/infra/sccache.sh"
    kio_configure_sccache_for_command "$@"
    kio_ensure_sccache_ready_for_command "$@"
    exit $?
    ;;
  -h|--help)
    usage
    exit 0
    ;;
esac

RESOURCE=work
BARRIER=
case "${1:-}" in
  --resource)
    RESOURCE=${2:-}
    case "$RESOURCE" in
      work|cargo|compiler) ;;
      *)
        printf 'error: %s --resource requires work, cargo, or compiler\n' \
          "$PROG" >&2
        exit 2
        ;;
    esac
    shift 2
    ;;
  --barrier)
    BARRIER=1
    shift
    ;;
esac

# A trusted caller may prove that the current compiler command cannot start a
# daemon and suppress adapter discovery for this invocation. Capture and remove
# the private marker before launching the target so nested compiler commands
# classify themselves independently.
READINESS_POLICY=${KIO_CI_SCHEDULE_READINESS:-AUTO}
unset KIO_CI_SCHEDULE_READINESS
case "$READINESS_POLICY" in
  ''|AUTO) READINESS_POLICY=AUTO ;;
  SKIP) ;;
  *)
    printf 'error: KIO_CI_SCHEDULE_READINESS must be AUTO, SKIP, or unset (got %s)\n' \
      "$READINESS_POLICY" >&2
    exit 2
    ;;
esac
[ "$READINESS_POLICY" = AUTO ] || [ "$RESOURCE" = compiler ] || {
  printf 'error: KIO_CI_SCHEDULE_READINESS=SKIP is valid only for compiler commands\n' >&2
  exit 2
}

[ "${1:-}" = -- ] || {
  printf 'error: %s expects -- before the command\n' "$PROG" >&2
  exit 2
}
shift
[ "$#" -gt 0 ] || {
  printf 'error: %s requires a command\n' "$PROG" >&2
  exit 2
}

case "${KIO_CI_SCHEDULE:-}" in
  ''|DISABLE) ;;
  *)
    printf 'error: KIO_CI_SCHEDULE must be DISABLE or unset (got %s)\n' \
      "$KIO_CI_SCHEDULE" >&2
    exit 2
    ;;
esac

READINESS_REQUIRED=
if [ "$RESOURCE" = compiler ] && [ "$READINESS_POLICY" = AUTO ]; then
  # The facade owns cache-wrapper discovery and registers the generic
  # post-admission hook. Configuration done here is inherited by the target.
  # shellcheck disable=SC1091
  . "$SCRIPT_DIR/infra/sccache.sh"
  kio_configure_sccache_for_command "$@"
  if kio_sccache_readiness_required_for_command "$@"; then
    READINESS_REQUIRED=1
  fi
fi

if [ "${KIO_CI_SCHEDULE:-}" = DISABLE ]; then
  if [ -n "$READINESS_REQUIRED" ]; then
    # A canonical inherited compiler lease has already established readiness.
    # Under the explicit native-scheduler bypass the facade validates only this
    # narrow reuse question; all other values conservatively probe again.
    case "${KIO_CI_SCHEDULE_HELD:-}" in
      compiler|work,compiler|cargo,compiler|work,cargo,compiler) ;;
      *) sh "$SCRIPT_DIR/schedule.sh" --readiness -- "$@" || exit $? ;;
    esac
  fi
  exec "$@"
fi

publish_schedule_root || exit $?
KIO_CI_SCHEDULER_BIN=$(resolve_scheduler_bin) || exit $?
export KIO_CI_SCHEDULER_BIN

case "$RESOURCE" in
  work)
    if [ -z "${KIO_CI_SCHEDULE_JOBS:-}" ]; then
      KIO_CI_SCHEDULE_JOBS=$(
        "$KIO_CI_SCHEDULER_BIN" available-parallelism
      ) || exit $?
      export KIO_CI_SCHEDULE_JOBS
    fi
    if [ -n "$BARRIER" ]; then
      if [ -n "$STDIN_CLOSED" ]; then
        exec "$KIO_CI_SCHEDULER_BIN" run --resource work --barrier \
          --jobs "$KIO_CI_SCHEDULE_JOBS" --stdin-closed -- "$@"
      fi
      exec "$KIO_CI_SCHEDULER_BIN" run --resource work --barrier \
        --jobs "$KIO_CI_SCHEDULE_JOBS" -- "$@"
    fi
    if [ -n "$STDIN_CLOSED" ]; then
      exec "$KIO_CI_SCHEDULER_BIN" run --resource work \
        --jobs "$KIO_CI_SCHEDULE_JOBS" --stdin-closed -- "$@"
    fi
    exec "$KIO_CI_SCHEDULER_BIN" run --resource work \
      --jobs "$KIO_CI_SCHEDULE_JOBS" -- "$@"
    ;;
  cargo)
    if [ -n "$STDIN_CLOSED" ]; then
      exec "$KIO_CI_SCHEDULER_BIN" run --resource cargo --stdin-closed -- "$@"
    fi
    exec "$KIO_CI_SCHEDULER_BIN" run --resource cargo -- "$@"
    ;;
  compiler)
    if [ -n "$STDIN_CLOSED" ]; then
      if [ -n "$READINESS_REQUIRED" ]; then
        exec "$KIO_CI_SCHEDULER_BIN" run --resource compiler --stdin-closed \
          --readiness-hook sh --readiness-hook-arg "$SCRIPT_DIR/schedule.sh" \
          -- "$@"
      fi
      exec "$KIO_CI_SCHEDULER_BIN" run --resource compiler --stdin-closed \
        -- "$@"
    fi
    if [ -n "$READINESS_REQUIRED" ]; then
      exec "$KIO_CI_SCHEDULER_BIN" run --resource compiler \
        --readiness-hook sh --readiness-hook-arg "$SCRIPT_DIR/schedule.sh" \
        -- "$@"
    fi
    exec "$KIO_CI_SCHEDULER_BIN" run --resource compiler -- "$@"
    ;;
esac
