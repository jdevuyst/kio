#!/bin/sh
# Verify keyed scheduler bootstrap, hot reuse, and the private Cargo boundary.

set -eu

# Wrapper identity is exercised causally below; ambient developer settings
# must not become an unrecorded fixture input.
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER \
  CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/scheduler-bootstrap-selftest.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

fixture=$scratch/repo
crate=$fixture/ci/infra/kio-ci-scheduler-rs
mkdir -p "$crate/src" "$scratch/bin" "$scratch/fail-bin"
real_git=$(command -v git)
cp "$REPO_ROOT/ci/schedule.sh" "$REPO_ROOT/ci/cargo.sh" "$fixture/ci/"
cp "$REPO_ROOT/ci/infra/sccache.sh" "$fixture/ci/infra/"
cp "$REPO_ROOT/ci/infra/kio-ci-scheduler-rs/bootstrap.sh" "$crate/"
printf '%s\n' '[package]' 'name = "kio-ci-scheduler"' 'version = "0.1.0"' \
  >"$crate/Cargo.toml"
printf '%s\n' 'version = 4' >"$crate/Cargo.lock"
printf '%s\n' 'fn main() {}' >"$crate/src/main.rs"
git -C "$fixture" init -q

cat >"$scratch/bin/git" <<'EOF'
#!/bin/sh
set -eu
if [ -n "${KIO_TEST_GIT_COMMON_DIR:-}" ]; then
  saw_rev_parse=
  saw_path_format=
  saw_common=
  for argument in "$@"; do
    [ "$argument" = rev-parse ] && saw_rev_parse=1
    [ "$argument" = --path-format=absolute ] && saw_path_format=1
    [ "$argument" = --git-common-dir ] && saw_common=1
  done
  if [ -n "$saw_rev_parse" ] && [ -n "$saw_path_format" ] && [ -n "$saw_common" ]; then
    printf '%s\n' "$KIO_TEST_GIT_COMMON_DIR"
    exit 0
  fi
fi
exec "$KIO_TEST_REAL_GIT" "$@"
EOF
chmod +x "$scratch/bin/git"

cat >"$scratch/bin/cygpath" <<'EOF'
#!/bin/sh
set -eu
[ "$#" -eq 2 ]
case "$1" in
  -u)
    [ "$2" = "$KIO_TEST_GIT_COMMON_DIR" ]
    printf '%s\n' "$KIO_TEST_GIT_COMMON_SHELL_DIR"
    ;;
  -au)
    case "$2" in
      /*) printf '%s\n' "$2" ;;
      *) printf '%s/%s\n' "${PWD%/}" "$2" ;;
    esac
    ;;
  -m) printf '%s\n' "$2" ;;
  *) exit 97 ;;
esac
EOF
chmod +x "$scratch/bin/cygpath"

cat >"$scratch/bin/rustc" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = -vV ] || exit 97
printf 'rustc %s\n' "${KIO_TEST_RUSTC_VERSION:-1.96.0}"
printf '%s\n' 'binary: rustc' 'commit-hash: fixture' \
  "host: ${KIO_TEST_RUSTC_HOST:-x86_64-unknown-linux-gnu}" \
  'release: fixture' 'LLVM version: fixture'
EOF
chmod +x "$scratch/bin/rustc"

cat >"$scratch/bin/cargo" <<'EOF'
#!/bin/sh
set -eu
printf 'cargo\n' >>"$KIO_TEST_CARGO_CALLS"
target=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --target ]; then
    target=$2
    shift 2
  else
    shift
  fi
done
[ -n "$target" ]
case "$target" in
  *-windows-*) executable=kio-ci-scheduler.exe ;;
  *) executable=kio-ci-scheduler ;;
esac
output=$CARGO_TARGET_DIR/$target/debug/$executable
lock=$CARGO_TARGET_DIR/.fixture-cargo-lock
mkdir -p "$CARGO_TARGET_DIR"
while ! (set -C; : >"$lock") 2>/dev/null; do sleep 0.01; done
trap 'rm -f "$lock"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ ! -x "$output" ] ||
   [ "$("$output" --build-id 2>/dev/null || true)" != "$KIO_CI_SCHEDULER_BUILD_ID" ]; then
  printf 'compile\n' >>"$KIO_TEST_COMPILE_CALLS"
  if [ -n "${KIO_TEST_MUTATE_SOURCE_ON_BUILD:-}" ] &&
     [ ! -e "$KIO_TEST_MUTATION_MARKER" ]; then
    : >"$KIO_TEST_MUTATION_MARKER"
    printf '%s\n' '// changed during scheduler build' \
      >>"$KIO_TEST_MUTATE_SOURCE_ON_BUILD"
  fi
  if [ -n "${KIO_TEST_MUTATE_CONFIG_ON_BUILD:-}" ] &&
     [ ! -e "$KIO_TEST_MUTATION_MARKER" ]; then
    : >"$KIO_TEST_MUTATION_MARKER"
    printf '%s\n' '# changed during scheduler build' \
      >>"$KIO_TEST_MUTATE_CONFIG_ON_BUILD"
  fi
  if [ -n "${KIO_TEST_MUTATE_CARGO_ON_BUILD:-}" ] &&
     [ ! -e "$KIO_TEST_MUTATION_MARKER" ]; then
    : >"$KIO_TEST_MUTATION_MARKER"
    printf '%s\n' '# changed during scheduler build' \
      >>"$KIO_TEST_MUTATE_CARGO_ON_BUILD"
  fi
  if [ -n "${KIO_TEST_MUTATE_WRAPPER_ON_BUILD:-}" ] &&
     [ ! -e "$KIO_TEST_MUTATION_MARKER" ]; then
    : >"$KIO_TEST_MUTATION_MARKER"
    printf '%s\n' '# changed during scheduler build' \
      >>"$KIO_TEST_MUTATE_WRAPPER_ON_BUILD"
  fi
  [ -z "${KIO_TEST_CARGO_SIGNAL:-}" ] || kill -s "$KIO_TEST_CARGO_SIGNAL" "$$"
  mkdir -p "${output%/*}"
  temp=$output.$$
  {
    printf '%s\n' '#!/bin/sh' 'set -eu' \
      "BUILD_ID=$KIO_CI_SCHEDULER_BUILD_ID"
    cat <<'SCHEDULER'
case "${1:-}" in
  --build-id)
    [ "$#" -eq 1 ] || exit 95
    printf '%s\n' "$BUILD_ID"
    exit 0
    ;;
  available-parallelism)
    [ "$#" -eq 1 ] || exit 95
    printf '%s\n' 2
    exit 0
    ;;
  self-test)
    [ "$#" -eq 1 ] || exit 95
    [ -z "${KIO_TEST_SELF_TESTED:-}" ] || : >"$KIO_TEST_SELF_TESTED"
    printf '%s\n' 'kio-ci-scheduler self-test: ok'
    exit 0
    ;;
  readiness)
    shift
    [ "${1:-}" = -- ] || exit 95
    shift
    [ "$#" -gt 0 ] || exit 95
    unset KIO_CI_SCHEDULE_HELD KIO_CI_SCHEDULE_LEASE_FDS
    exec "$@"
    ;;
  run) shift ;;
  *) exit 96 ;;
esac

resource=
hook=
hook_arg_count=0
while [ "${1:-}" != -- ]; do
  case "${1:-}" in
    --resource)
      [ -z "$resource" ] || exit 95
      resource=${2:-}
      shift 2
      ;;
    --readiness-hook)
      [ -z "$hook" ] || exit 95
      hook=${2:-}
      shift 2
      ;;
    --readiness-hook-arg)
      hook_arg_count=$((hook_arg_count + 1))
      eval "hook_arg_$hook_arg_count=\$2"
      shift 2
      ;;
    --stdin-closed)
      shift
      ;;
    *) exit 95 ;;
  esac
done
[ "$resource" = compiler ] || exit 93
[ "${1:-}" = -- ] || exit 95
shift
[ "$#" -gt 0 ] || exit 95
if [ "$hook_arg_count" -gt 0 ] && [ -z "$hook" ]; then
  exit 95
fi
if [ -n "$hook" ]; then
  hook_arg_total=$hook_arg_count
  set -- --compiler-readiness -- "$@"
  while [ "$hook_arg_count" -gt 0 ]; do
    eval "hook_arg=\${hook_arg_$hook_arg_count}"
    set -- "$hook_arg" "$@"
    hook_arg_count=$((hook_arg_count - 1))
  done
  "$hook" "$@"
  while [ "$hook_arg_total" -gt 0 ]; do
    shift
    hook_arg_total=$((hook_arg_total - 1))
  done
  shift 2
fi
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  ,,) KIO_CI_SCHEDULE_HELD=compiler ;;
  *) KIO_CI_SCHEDULE_HELD=${KIO_CI_SCHEDULE_HELD},compiler ;;
esac
export KIO_CI_SCHEDULE_HELD
exec "$@"
SCHEDULER
  } >"$temp"
  chmod +x "$temp"
  mv "$temp" "$output"
fi
EOF
chmod +x "$scratch/bin/cargo"

calls=$scratch/cargo-calls
compiles=$scratch/compile-calls
prepare() (
  # This helper exercises a private repository/bootstrap fixture. A broad gate
  # legitimately exports its own live scheduler context to every task; do not
  # let that outer hot path short-circuit the fixture's cold resolver.
  unset \
    KIO_CI_SCHEDULE \
    KIO_CI_SCHEDULE_DIR \
    KIO_CI_SCHEDULE_HELD \
    KIO_CI_SCHEDULE_JOBS \
    KIO_CI_SCHEDULE_COMPILER_JOBS \
    KIO_CI_SCHEDULE_LEASE_FDS \
    KIO_CI_SCHEDULE_READINESS \
    KIO_CI_SCHEDULER_BIN \
    KIO_CI_SERIALIZE_CARGO
  PATH="${KIO_TEST_PATH_PREFIX:+$KIO_TEST_PATH_PREFIX:}$scratch/bin:$PATH" \
    KIO_TEST_REAL_GIT="$real_git" \
    KIO_TEST_CARGO_CALLS="$calls" \
    KIO_TEST_COMPILE_CALLS="$compiles" \
    sh "$fixture/ci/schedule.sh" --prepare
)
line_count() {
  [ -f "$1" ] || { printf '0\n'; return; }
  wc -l <"$1" | tr -d ' '
}

cold_bin=$(KIO_CI_SCHEDULER_BIN=$scratch/bin/rustc \
  KIO_CI_SCHEDULE_DIR=$scratch/outer-state \
  KIO_CI_SCHEDULE_HELD=work \
  KIO_CI_SCHEDULE_LEASE_FDS=999 \
  KIO_CI_SERIALIZE_CARGO=1 \
  prepare)
[ -x "$cold_bin" ] || {
  printf 'scheduler-bootstrap-selftest: cold prepare returned no executable\n' >&2
  exit 1
}
if [ "$(line_count "$calls")" -ne 1 ] || [ "$(line_count "$compiles")" -ne 1 ]; then
  printf 'scheduler-bootstrap-selftest: cold prepare did not run exactly one Cargo build\n' >&2
  exit 1
fi
warm_bin=$(prepare)
if [ "$warm_bin" != "$cold_bin" ] || [ "$(line_count "$calls")" -ne 1 ]; then
  printf 'scheduler-bootstrap-selftest: warm prepare invoked Cargo or changed identity\n' >&2
  exit 1
fi

windows_common_shell=$scratch/windows-linked-common
mkdir -p "$windows_common_shell"
windows_bin=$(OS=Windows_NT MSYSTEM=MINGW64 \
  KIO_TEST_GIT_COMMON_DIR='C:/linked/common.git' \
  KIO_TEST_GIT_COMMON_SHELL_DIR="$windows_common_shell" \
  KIO_TEST_RUSTC_HOST=x86_64-pc-windows-msvc \
  prepare)
case "$windows_bin" in
  "$windows_common_shell"/kio-ci-scheduler/bin/*/kio-ci-scheduler.exe) ;;
  *)
    printf 'scheduler-bootstrap-selftest: Windows Git-common path was not normalized\n' >&2
    exit 1
    ;;
esac

for tool in git rustc cargo; do
  cat >"$scratch/fail-bin/$tool" <<'EOF'
#!/bin/sh
: >"$KIO_TEST_UNEXPECTED_TOOL"
exit 99
EOF
  chmod +x "$scratch/fail-bin/$tool"
done
hot_prepared=$(PATH="$scratch/fail-bin:$PATH" \
  KIO_TEST_UNEXPECTED_TOOL="$scratch/unexpected-tool" \
  KIO_CI_SCHEDULER_BIN="$cold_bin" \
  sh "$fixture/ci/schedule.sh" --prepare)
if [ "$hot_prepared" != "$cold_bin" ] || [ -e "$scratch/unexpected-tool" ]; then
  printf 'scheduler-bootstrap-selftest: exported hot path performed discovery\n' >&2
  exit 1
fi
self_test_output=$scratch/self-test-output
PATH="$scratch/fail-bin:$PATH" \
  KIO_TEST_UNEXPECTED_TOOL="$scratch/unexpected-tool" \
  KIO_TEST_SELF_TESTED="$scratch/self-tested" \
  KIO_CI_SCHEDULER_BIN="$cold_bin" \
  sh "$fixture/ci/schedule.sh" --self-test >"$self_test_output"
if [ ! -e "$scratch/self-tested" ] ||
   [ "$(cat "$self_test_output")" != 'kio-ci-scheduler self-test: ok' ] ||
   [ -e "$scratch/unexpected-tool" ]; then
  printf 'scheduler-bootstrap-selftest: self-test hot path diverged from the native scheduler\n' >&2
  exit 1
fi
parallelism=$(PATH="$scratch/fail-bin:$PATH" \
  KIO_TEST_UNEXPECTED_TOOL="$scratch/unexpected-tool" \
  KIO_CI_SCHEDULER_BIN="$cold_bin" \
  sh "$fixture/ci/schedule.sh" --available-parallelism)
if [ "$parallelism" != 2 ] || [ -e "$scratch/unexpected-tool" ]; then
  printf 'scheduler-bootstrap-selftest: parallelism hot path performed discovery or changed output\n' >&2
  exit 1
fi
cat >"$scratch/mark" <<'EOF'
#!/bin/sh
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) : >"$KIO_TEST_ADMITTED" ;;
  *) exit 94 ;;
esac
EOF
chmod +x "$scratch/mark"
PATH="$scratch/fail-bin:$PATH" \
  KIO_TEST_UNEXPECTED_TOOL="$scratch/unexpected-tool" \
  KIO_TEST_ADMITTED="$scratch/admitted" \
  KIO_CI_SCHEDULER_BIN="$cold_bin" \
  KIO_CI_SCHEDULE_DIR="$scratch/state" \
  sh "$fixture/ci/schedule.sh" --resource compiler -- "$scratch/mark"
if [ ! -e "$scratch/admitted" ] || [ -e "$scratch/unexpected-tool" ]; then
  printf 'scheduler-bootstrap-selftest: compiler hot path hashed or invoked Cargo\n' >&2
  exit 1
fi

cat >"$scratch/readiness-hook" <<'EOF'
#!/bin/sh
set -eu
[ "$#" -eq 5 ]
[ "$1" = 'prefix one' ]
[ "$2" = 'prefix two' ]
[ "$3" = --compiler-readiness ]
[ "$4" = -- ]
[ "$5" = "$KIO_TEST_EXPECTED_TARGET" ]
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) exit 93 ;;
  *) ;;
esac
: >"$KIO_TEST_HOOKED"
EOF
chmod +x "$scratch/readiness-hook"
KIO_TEST_EXPECTED_TARGET="$scratch/mark" \
  KIO_TEST_HOOKED="$scratch/vector-hooked" \
  KIO_TEST_ADMITTED="$scratch/vector-admitted" \
  "$cold_bin" run --resource compiler \
    --readiness-hook "$scratch/readiness-hook" \
    --readiness-hook-arg 'prefix one' \
    --readiness-hook-arg 'prefix two' \
    -- "$scratch/mark"
if [ ! -e "$scratch/vector-hooked" ] || [ ! -e "$scratch/vector-admitted" ]; then
  printf 'scheduler-bootstrap-selftest: generic readiness-hook vector was not preserved\n' >&2
  exit 1
fi

windows_sccache=$scratch/bin/'C:\Tools\ScCaChE.ExE'
cat >"$windows_sccache" <<'EOF'
#!/bin/sh
set -eu
if [ "${1:-}" = --dist-status ]; then
  [ "${SCCACHE_IDLE_TIMEOUT:-}" = 0 ]
  [ -z "${KIO_CI_SCHEDULE_HELD:-}" ]
  [ -z "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ]
  : >"$KIO_TEST_SCCACHE_READY"
  exit 0
fi
[ "${1:-}" = rustc ]
: >"$KIO_TEST_SCCACHE_RAN"
EOF
chmod +x "$windows_sccache"
KIO_TEST_SCCACHE_READY="$scratch/sccache-ready" \
  KIO_CI_SCHEDULER_BIN="$cold_bin" \
  KIO_CI_SCHEDULE_HELD=work \
  KIO_CI_SCHEDULE_LEASE_FDS=7 \
  sh "$fixture/ci/schedule.sh" --readiness -- "$windows_sccache" rustc
[ -e "$scratch/sccache-ready" ] || {
  printf 'scheduler-bootstrap-selftest: isolated Windows sccache readiness was skipped\n' >&2
  exit 1
}
KIO_TEST_SCCACHE_READY="$scratch/sccache-ready-again" \
  KIO_TEST_SCCACHE_RAN="$scratch/sccache-ran" \
  KIO_CI_SCHEDULER_BIN="$cold_bin" \
  KIO_CI_SCHEDULE_DIR="$scratch/state" \
  sh "$fixture/ci/schedule.sh" --resource compiler -- \
    "$windows_sccache" rustc
if [ ! -e "$scratch/sccache-ready-again" ] || [ ! -e "$scratch/sccache-ran" ]; then
  printf 'scheduler-bootstrap-selftest: Windows sccache command bypassed readiness\n' >&2
  exit 1
fi

printf '%s\n' 'fn main() { let _ = 1; }' >"$crate/src/main.rs"
content_bin=$(prepare)
if [ "$content_bin" = "$cold_bin" ] || [ "$(line_count "$calls")" -ne 3 ]; then
  printf 'scheduler-bootstrap-selftest: source content did not invalidate the key\n' >&2
  exit 1
fi
flag_bin=$(RUSTFLAGS=-Cdebuginfo=0 prepare)
if [ "$flag_bin" = "$content_bin" ] || [ "$(line_count "$calls")" -ne 4 ]; then
  printf 'scheduler-bootstrap-selftest: build flags did not invalidate the key\n' >&2
  exit 1
fi
toolchain_bin=$(KIO_TEST_RUSTC_VERSION=1.96.1 prepare)
if [ "$toolchain_bin" = "$content_bin" ] || [ "$(line_count "$calls")" -ne 5 ]; then
  printf 'scheduler-bootstrap-selftest: toolchain did not invalidate the key\n' >&2
  exit 1
fi

cp "$scratch/bin/rustc" "$crate/relative-rustc"
before=$(line_count "$calls")
relative_rustc_bin=$(RUSTC=./relative-rustc \
  KIO_TEST_RUSTC_VERSION=1.96.2 prepare)
if [ "$relative_rustc_bin" = "$content_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: relative RUSTC did not resolve from the build directory\n' >&2
  exit 1
fi

relative_cargo_home=$crate/relative-cargo-home
mkdir -p "$relative_cargo_home"
printf '%s\n' '[build]' 'incremental = false' \
  >"$relative_cargo_home/config.toml"
before=$(line_count "$calls")
relative_home_bin=$(CARGO_HOME=relative-cargo-home prepare)
if [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: relative CARGO_HOME was not keyed from the build directory\n' >&2
  exit 1
fi
printf '%s\n' '# relative home content change' \
  >>"$relative_cargo_home/config.toml"
before=$(line_count "$calls")
changed_relative_home_bin=$(CARGO_HOME=relative-cargo-home prepare)
if [ "$changed_relative_home_bin" = "$relative_home_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: relative CARGO_HOME content did not invalidate the key\n' >&2
  exit 1
fi

windows_relative_home=$crate/relative-windows-cargo-home
mkdir -p "$windows_relative_home"
printf '%s\n' '[build]' 'incremental = false' \
  >"$windows_relative_home/config.toml"
before=$(line_count "$calls")
windows_relative_home_bin=$(OS=Windows_NT MSYSTEM=MINGW64 \
  KIO_TEST_GIT_COMMON_DIR='C:/linked/common.git' \
  KIO_TEST_GIT_COMMON_SHELL_DIR="$windows_common_shell" \
  KIO_TEST_RUSTC_HOST=x86_64-pc-windows-msvc \
  CARGO_HOME=relative-windows-cargo-home prepare)
if [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: Windows relative CARGO_HOME was not keyed from the build directory\n' >&2
  exit 1
fi
printf '%s\n' '# Windows relative home content change' \
  >>"$windows_relative_home/config.toml"
before=$(line_count "$calls")
windows_relative_home_bin_again=$(OS=Windows_NT MSYSTEM=MINGW64 \
  KIO_TEST_GIT_COMMON_DIR='C:/linked/common.git' \
  KIO_TEST_GIT_COMMON_SHELL_DIR="$windows_common_shell" \
  KIO_TEST_RUSTC_HOST=x86_64-pc-windows-msvc \
  CARGO_HOME=relative-windows-cargo-home prepare)
if [ "$windows_relative_home_bin_again" = "$windows_relative_home_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: Windows relative CARGO_HOME content did not invalidate the key\n' >&2
  exit 1
fi

caller=$scratch/caller
mkdir -p "$caller/relative-bin" "$crate/relative-bin"
cp "$scratch/bin/cargo" "$caller/relative-bin/cargo"
cp "$scratch/bin/cargo" "$crate/relative-bin/cargo"
printf '%s\n' '# relative Cargo identity' \
  >>"$caller/relative-bin/cargo"
printf '%s\n' '# relative Cargo identity' \
  >>"$crate/relative-bin/cargo"
before=$(line_count "$calls")
relative_path_bin=$(cd "$caller" && KIO_TEST_PATH_PREFIX=relative-bin prepare)
if [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: build-relative Cargo path did not produce a keyed build\n' >&2
  exit 1
fi
printf '%s\n' '# caller-only Cargo content change' \
  >>"$caller/relative-bin/cargo"
before=$(line_count "$calls")
relative_path_bin_again=$(cd "$caller" && KIO_TEST_PATH_PREFIX=relative-bin prepare)
if [ "$relative_path_bin_again" != "$relative_path_bin" ] ||
   [ "$(line_count "$calls")" -ne "$before" ]; then
  printf 'scheduler-bootstrap-selftest: caller-relative Cargo contaminated the build key\n' >&2
  exit 1
fi

printf '%s\n' '# cargo identity change' >>"$scratch/bin/cargo"
before=$(line_count "$calls")
cargo_bin=$(prepare)
if [ "$cargo_bin" = "$content_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: Cargo identity did not invalidate the key\n' >&2
  exit 1
fi
mkdir -p "$fixture/.cargo"
printf '%s\n' '[build]' 'rustflags = ["-Cdebuginfo=0"]' \
  >"$fixture/.cargo/config.toml"
before=$(line_count "$calls")
config_bin=$(prepare)
if [ "$config_bin" = "$cargo_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: Cargo config did not invalidate the key\n' >&2
  exit 1
fi

wrapper_one=$scratch/bin/bootstrap-wrapper-one
wrapper_two=$scratch/bin/bootstrap-wrapper-two
printf '%s\n' '#!/bin/sh' 'exit 0' >"$wrapper_one"
cp "$wrapper_one" "$wrapper_two"
chmod +x "$wrapper_one" "$wrapper_two"

before=$(line_count "$calls")
wrapper_one_bin=$(RUSTC_WRAPPER=$wrapper_one prepare)
if [ "$wrapper_one_bin" = "$config_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: RUSTC_WRAPPER setting did not invalidate the key\n' >&2
  exit 1
fi

before=$(line_count "$calls")
wrapper_two_bin=$(RUSTC_WRAPPER=$wrapper_two prepare)
if [ "$wrapper_two_bin" = "$wrapper_one_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: changed RUSTC_WRAPPER value did not invalidate the key\n' >&2
  exit 1
fi

before=$(line_count "$calls")
workspace_wrapper_bin=$(RUSTC_WORKSPACE_WRAPPER=$wrapper_one prepare)
if [ "$workspace_wrapper_bin" = "$wrapper_one_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: RUSTC_WORKSPACE_WRAPPER setting did not invalidate the key\n' >&2
  exit 1
fi

printf '%s\n' '# same path, changed wrapper content' >>"$wrapper_one"
before=$(line_count "$calls")
mutated_wrapper_bin=$(RUSTC_WRAPPER=$wrapper_one prepare)
if [ "$mutated_wrapper_bin" = "$wrapper_one_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: in-place wrapper change did not invalidate the key\n' >&2
  exit 1
fi

before=$(line_count "$calls")
cargo_wrapper_bin=$(CARGO_BUILD_RUSTC_WRAPPER=$wrapper_two prepare)
if [ "$cargo_wrapper_bin" = "$config_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: Cargo build wrapper setting did not invalidate the key\n' >&2
  exit 1
fi
printf '%s\n' '# same Cargo build wrapper path, changed content' >>"$wrapper_two"
before=$(line_count "$calls")
mutated_cargo_wrapper_bin=$(CARGO_BUILD_RUSTC_WRAPPER=$wrapper_two prepare)
if [ "$mutated_cargo_wrapper_bin" = "$cargo_wrapper_bin" ] ||
   [ "$(line_count "$calls")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: in-place Cargo build wrapper change did not invalidate the key\n' >&2
  exit 1
fi

printf '%s\n' 'fn main() { let _ = 2; }' >"$crate/src/main.rs"
before=$(line_count "$compiles")
prepare >"$scratch/concurrent-one" &
first_pid=$!
prepare >"$scratch/concurrent-two" &
second_pid=$!
wait "$first_pid"
wait "$second_pid"
if [ "$(cat "$scratch/concurrent-one")" != "$(cat "$scratch/concurrent-two")" ] ||
   [ "$(line_count "$compiles")" -ne $((before + 1)) ]; then
  printf 'scheduler-bootstrap-selftest: concurrent cold prepare was not single-build\n' >&2
  exit 1
fi

printf '%s\n' 'fn main() { let _ = 3; }' >"$crate/src/main.rs"
race_calls=$(line_count "$calls")
race_compiles=$(line_count "$compiles")
race_bin=$(KIO_TEST_MUTATE_SOURCE_ON_BUILD=$crate/src/main.rs \
  KIO_TEST_MUTATION_MARKER=$scratch/source-mutated-during-build prepare)
if [ ! -e "$scratch/source-mutated-during-build" ] ||
   [ "$(line_count "$calls")" -ne $((race_calls + 2)) ] ||
   [ "$(line_count "$compiles")" -ne $((race_compiles + 2)) ] ||
   [ ! -x "$race_bin" ]; then
  printf 'scheduler-bootstrap-selftest: source/build race published a mixed-generation key\n' >&2
  exit 1
fi

printf '%s\n' 'fn main() { let _ = 4; }' >"$crate/src/main.rs"
race_calls=$(line_count "$calls")
race_compiles=$(line_count "$compiles")
race_bin=$(KIO_TEST_MUTATE_CONFIG_ON_BUILD=$fixture/.cargo/config.toml \
  KIO_TEST_MUTATION_MARKER=$scratch/config-mutated-during-build prepare)
if [ ! -e "$scratch/config-mutated-during-build" ] ||
   [ "$(line_count "$calls")" -ne $((race_calls + 2)) ] ||
   [ "$(line_count "$compiles")" -ne $((race_compiles + 2)) ] ||
   [ ! -x "$race_bin" ]; then
  printf 'scheduler-bootstrap-selftest: config/build race published a mixed-generation key\n' >&2
  exit 1
fi

printf '%s\n' 'fn main() { let _ = 5; }' >"$crate/src/main.rs"
race_calls=$(line_count "$calls")
race_compiles=$(line_count "$compiles")
race_bin=$(KIO_TEST_MUTATE_CARGO_ON_BUILD=$scratch/bin/cargo \
  KIO_TEST_MUTATION_MARKER=$scratch/cargo-mutated-during-build prepare)
if [ ! -e "$scratch/cargo-mutated-during-build" ] ||
   [ "$(line_count "$calls")" -ne $((race_calls + 2)) ] ||
   [ "$(line_count "$compiles")" -ne $((race_compiles + 2)) ] ||
   [ ! -x "$race_bin" ]; then
  printf 'scheduler-bootstrap-selftest: Cargo/build race published a mixed-generation key\n' >&2
  exit 1
fi

printf '%s\n' 'fn main() { let _ = 6; }' >"$crate/src/main.rs"
race_calls=$(line_count "$calls")
race_compiles=$(line_count "$compiles")
race_bin=$(RUSTC_WRAPPER=$wrapper_one \
  KIO_TEST_MUTATE_WRAPPER_ON_BUILD=$wrapper_one \
  KIO_TEST_MUTATION_MARKER=$scratch/wrapper-mutated-during-build prepare)
if [ ! -e "$scratch/wrapper-mutated-during-build" ] ||
   [ "$(line_count "$calls")" -ne $((race_calls + 2)) ] ||
   [ "$(line_count "$compiles")" -ne $((race_compiles + 2)) ] ||
   [ ! -x "$race_bin" ]; then
  printf 'scheduler-bootstrap-selftest: wrapper/build race published a mixed-generation key\n' >&2
  exit 1
fi

before=$(line_count "$calls")
bad_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
if (
  cd "$fixture"
  PATH="$scratch/bin:$PATH" \
    KIO_TEST_CARGO_CALLS="$calls" KIO_TEST_COMPILE_CALLS="$compiles" \
    CARGO_TARGET_DIR="$scratch/private-target" \
    KIO_CI_SCHEDULER_BUILD_ID="$bad_id" \
    sh "$fixture/ci/cargo.sh" --bootstrap-kio-ci-scheduler \
      build --locked --quiet --bin kio-ci-scheduler \
      --target x86_64-unknown-linux-gnu
) 2>/dev/null; then
  printf 'scheduler-bootstrap-selftest: private Cargo mode accepted the wrong cwd\n' >&2
  exit 1
fi
if (
  cd "$crate"
  PATH="$scratch/bin:$PATH" \
    KIO_TEST_CARGO_CALLS="$calls" KIO_TEST_COMPILE_CALLS="$compiles" \
    CARGO_TARGET_DIR="$scratch/private-target" \
    KIO_CI_SCHEDULER_BUILD_ID="$bad_id" \
    sh "$fixture/ci/cargo.sh" --bootstrap-kio-ci-scheduler build --locked
) 2>/dev/null; then
  printf 'scheduler-bootstrap-selftest: private Cargo mode accepted arbitrary args\n' >&2
  exit 1
fi
[ "$(line_count "$calls")" -eq "$before" ] || {
  printf 'scheduler-bootstrap-selftest: rejected private Cargo mode reached Cargo\n' >&2
  exit 1
}

set +e
KIO_TEST_CARGO_SIGNAL=TERM \
  KIO_TEST_CARGO_CALLS="$scratch/signal-calls" \
  KIO_TEST_COMPILE_CALLS="$scratch/signal-compiles" \
  CARGO_TARGET_DIR="$scratch/signal-target" \
  KIO_CI_SCHEDULER_BUILD_ID=interrupted \
  sh "$scratch/bin/cargo" --target fixture-host
signal_status=$?
set -e
if [ "$signal_status" -ne 143 ] ||
   [ -e "$scratch/signal-target/.fixture-cargo-lock" ] ||
   [ -e "$scratch/signal-target/fixture-host/debug/kio-ci-scheduler" ]; then
  printf 'scheduler-bootstrap-selftest: interrupted lock owner continued publication or retained its lock\n' >&2
  exit 1
fi

printf 'scheduler-bootstrap-selftest: ok\n'
