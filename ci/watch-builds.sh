#!/bin/sh
#
# Keep the local kio-rs build artifact warm while editing.
#
# This is a local development helper, not a CI gate. It watches the kio-rs
# compiler crate and reruns only cargo build after source changes settle. It
# intentionally does not run tests, clippy, format checks, golden tests, or
# generative tests.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
PROG=$0

DEBOUNCE=2
POLL=1
RUN_ONCE=
WORKER=

usage() {
  cat <<EOF
Usage: sh $PROG [--debounce=<seconds>] [--poll=<seconds>] [--once]

Continuously rebuild the kio-rs debug binaries that local corpus checks consume.
The watcher debounces source edits and cancels an in-flight stale build before
starting the next one. Cargo's own locks coordinate with any simultaneous local
CI run. Builds go through ci/cargo.sh and inherit the caller's Cargo environment
including RUSTC_WRAPPER and SCCACHE_DIR. If the kio-rs crate disappears, the
watcher exits and terminates any in-flight build.

Options:
  --debounce=<seconds>  Wait this long after the last edit before rebuilding
                        (default: $DEBOUNCE).
  --poll=<seconds>      Source scan interval (default: $POLL).
  --once                Run one kio-rs build pass and exit.
  -h, --help            Show this help and exit.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --debounce=*)
      DEBOUNCE=${1#--debounce=}
      ;;
    --poll=*)
      POLL=${1#--poll=}
      ;;
    --once)
      RUN_ONCE=1
      ;;
    --worker)
      WORKER=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'error: unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
  shift
done

case "$DEBOUNCE" in
  ''|*[!0-9]*)
    printf 'error: --debounce must be a non-negative integer number of seconds\n' >&2
    exit 2
    ;;
esac
case "$POLL" in
  ''|*[!0-9]*)
    printf 'error: --poll must be a positive integer number of seconds\n' >&2
    exit 2
    ;;
  0)
    printf 'error: --poll must be a positive integer number of seconds\n' >&2
    exit 2
    ;;
esac

run_builds() {
  if ! crate_present; then
    printf '%s: kio-rs crate no longer present; exiting\n' "$PROG"
    exit 0
  fi
  start=$(date +%s)
  printf '%s: build start kio-rs\n' "$PROG"
  (
    cd "$REPO_ROOT/kio-rs"
    sh "$REPO_ROOT/ci/cargo.sh" build
  )
  end=$(date +%s)
  printf '%s: build pass kio-rs (%ss wall)\n' "$PROG" "$((end - start))"
}

crate_present() {
  [ -d "$REPO_ROOT/kio-rs/src" ] || return 1
  [ -f "$REPO_ROOT/kio-rs/Cargo.toml" ] || return 1
}

exit_if_crate_removed() {
  if ! crate_present; then
    printf '%s: kio-rs crate no longer present; exiting\n' "$PROG"
    exit 0
  fi
}

if [ -n "$WORKER" ]; then
  run_builds
  exit 0
fi

if [ -n "$RUN_ONCE" ]; then
  run_builds
  exit 0
fi

watch_fingerprint() {
  exit_if_crate_removed
  {
    find "$REPO_ROOT/kio-rs/src" -type f -name '*.rs' -exec cksum {} \;
    find "$REPO_ROOT/kio-rs" -maxdepth 1 -type f \
      \( -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'build.rs' \) -exec cksum {} \;
    cksum "$REPO_ROOT/ci/cargo.sh"
  } | sort | cksum
}

build_pid=
build_pgid=

build_running() {
  [ -n "${build_pid:-}" ] && kill -0 "$build_pid" 2>/dev/null
}

start_build() {
  if command -v setsid >/dev/null 2>&1; then
    setsid sh "$PROG" --worker &
    build_pid=$!
    build_pgid=$build_pid
  else
    sh "$PROG" --worker &
    build_pid=$!
    build_pgid=
  fi
}

stop_build() {
  if ! build_running; then
    build_pid=
    build_pgid=
    return 0
  fi
  printf '%s: source changed; cancelling stale build\n' "$PROG"
  if [ -n "$build_pgid" ]; then
    kill -TERM "-$build_pgid" 2>/dev/null || kill -TERM "$build_pid" 2>/dev/null || true
  else
    pkill -TERM -P "$build_pid" 2>/dev/null || true
    kill -TERM "$build_pid" 2>/dev/null || true
  fi
  wait "$build_pid" 2>/dev/null || true
  build_pid=
  build_pgid=
}

on_exit() {
  stop_build
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

last_fp=$(watch_fingerprint)
last_change=$(date +%s)
pending=1

printf '%s: watching Rust build inputs; debounce=%ss poll=%ss\n' "$PROG" "$DEBOUNCE" "$POLL"
printf '%s: press Ctrl-C to stop\n' "$PROG"

while :; do
  exit_if_crate_removed
  now=$(date +%s)
  fp=$(watch_fingerprint)
  if [ "$fp" != "$last_fp" ]; then
    last_fp=$fp
    last_change=$now
    pending=1
    stop_build
  fi

  if build_running; then
    :
  elif [ -n "${build_pid:-}" ]; then
    if wait "$build_pid"; then
      :
    else
      printf '%s: kio-rs build failed; waiting for the next edit\n' "$PROG" >&2
    fi
    build_pid=
    build_pgid=
  elif [ "$pending" = 1 ] && [ "$((now - last_change))" -ge "$DEBOUNCE" ]; then
    pending=
    start_build
  fi

  sleep "$POLL"
done
