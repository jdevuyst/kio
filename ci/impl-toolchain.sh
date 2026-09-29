#!/bin/sh
#
# Select CI implementation shards and install the host-language toolchains they need.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)

CORE_IMPLS="kio@js kio@ts kio@python kio@rust"
EXTRA_IMPLS="kio@go kio@java kio@swift kio@haskell"

CORE_MISE_TOOLS="node rust tree-sitter actionlint zizmor sccache shellcheck python pipx yamllint ripgrep aqua:wasm-bindgen/wasm-pack npm:typescript npm:pyright npm:markdownlint-cli2 github:mszostok/codeowners-validator[exe=codeowners-validator]"
REPORT_MISE_TOOLS="cargo:cargo-fuzz cargo:cargo-mutants cargo:cargo-llvm-cov cargo:cargo-audit"

usage() {
  cat <<EOF
Usage: sh ci/impl-toolchain.sh <command> [args]

Commands:
  core-impls
      Print the always-on implementation list.
  extra-impls
      Print the extra-backend implementation list.
  impls
      Print every known implementation selector.
  installed-impls
      Print known implementation selectors whose runner tools are on PATH.
  tooling-impls <impl-list>
      Print the selected implementations plus toolchains already on PATH
      that availability-driven checks use, including compile-only Java.
  select [--extra-count=<N>|--count=<N>] [--seed=<S>]
      Print a comma-separated impl list. Core impls are always included;
      extras are selected as a seeded-random shard. By default, select
      3 extras. --count is a backwards-compatible total impl count;
      prefer --extra-count for CI and docs.
  core-mise-tools
      Print the mise tools baked into the core devcontainer image.
  report-mise-tools
      Print optional Cargo-backed tools for reports, audits, and upgrades.
  install-report-tools
      Install optional Cargo-backed tools for reports, audits, and upgrades.
  extra-mise-tools <impl-list>
      Print the mise tools needed beyond core for the impl list.
  extra-system-packages <impl-list>
      Print the mise bootstrap system packages needed beyond core for
      the impl list.
  install <impl-list>
      Install the extra system packages and mise tools needed by the impl
      list. Intended for Linux CI jobs running inside the core image.
EOF
}

join_csv() {
  awk 'NF { if (seen) printf ","; printf "%s", $0; seen = 1 } END { printf "\n" }'
}

print_words() {
  for word in "$@"; do
    printf '%s\n' "$word"
  done
}

print_core_impls() {
  # shellcheck disable=SC2086
  print_words $CORE_IMPLS
}

print_extra_impls() {
  # shellcheck disable=SC2086
  print_words $EXTRA_IMPLS
}

print_all_impls() {
  {
    print_core_impls
    print_extra_impls
  } | join_csv
}

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

have_any_cmd() {
  for cmd in "$@"; do
    have_cmd "$cmd" && return 0
  done
  return 1
}

impl_tools_installed() {
  case "$1" in
    kio@js) have_cmd node ;;
    kio@ts) have_cmd node && have_cmd tsc ;;
    kio@rust) have_cmd cargo && have_cmd rustc ;;
    kio@python) have_any_cmd python3 python ;;
    kio@go) have_cmd go ;;
    kio@java) have_cmd java && have_cmd javac ;;
    kio@swift) have_cmd swiftc ;;
    kio@haskell) have_cmd ghc ;;
    *) return 1 ;;
  esac
}

print_installed_impls() {
  {
    print_core_impls
    print_extra_impls
  } | while IFS= read -r impl; do
    if impl_tools_installed "$impl"; then
      printf '%s\n' "$impl"
    fi
  done | join_csv
}

print_core_mise_tools() {
  # shellcheck disable=SC2086
  print_words $CORE_MISE_TOOLS
}

print_report_mise_tools() {
  # shellcheck disable=SC2086
  print_words $REPORT_MISE_TOOLS
}

install_report_mise_tools() {
  # shellcheck disable=SC2086
  mise -E optional install --locked -y rust $REPORT_MISE_TOOLS
}

normalize_impl_args() {
  [ $# -gt 0 ] || return 0
  printf '%s\n' "$@" | tr ',' '\n' | awk 'NF { print }'
}

print_tooling_impls() {
  selected=$(normalize_impl_args "$@")
  for impl in $selected; do
    validate_impl "$impl"
  done
  {
    printf '%s\n' "$selected"
    print_installed_impls | tr ',' '\n'
    # Host snippets need javac even when no Java runtime is available.
    if have_cmd javac; then
      printf '%s\n' kio@java
    fi
  } | awk 'NF && !seen[$0]++' | join_csv
}

known_impl() {
  case " $CORE_IMPLS $EXTRA_IMPLS " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

validate_impl() {
  if ! known_impl "$1"; then
    known=$({ print_core_impls; print_extra_impls; } | join_csv)
    printf 'error: unknown impl %s (known: %s)\n' "$1" "$known" >&2
    exit 2
  fi
}

add_word() {
  word=$1
  case " $WORDS " in
    *" $word "*) ;;
    *) WORDS="${WORDS:+$WORDS }$word" ;;
  esac
}

emit_extra_mise_tools() {
  WORDS=
  for impl in $(normalize_impl_args "$@"); do
    validate_impl "$impl"
    case "$impl" in
      kio@go) add_word go ;;
      kio@java) add_word java ;;
      kio@swift) add_word swift ;;
      kio@haskell) add_word ghcup ;;
      *) ;;
    esac
  done
  # shellcheck disable=SC2086
  print_words $WORDS
}

emit_extra_system_packages() {
  WORDS=
  for impl in $(normalize_impl_args "$@"); do
    validate_impl "$impl"
    case "$impl" in
      kio@swift)
        for pkg in \
          binutils libc6-dev libcurl4-openssl-dev libedit2 libgcc-13-dev \
          libncurses-dev libpython3-dev libsqlite3-0 libstdc++-13-dev \
          libxml2-dev libz3-dev tzdata zlib1g-dev
        do
          add_word "apt:$pkg"
        done
        ;;
      kio@haskell)
        add_word apt:libgmp-dev
        add_word apt:libtinfo6
        ;;
      *) ;;
    esac
  done
  # shellcheck disable=SC2086
  print_words $WORDS
}

as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    printf 'error: need root privileges for: %s\n' "$*" >&2
    exit 2
  fi
}

install_impl_tools() {
  impls=$(normalize_impl_args "$@")
  [ -n "$impls" ] || { printf 'error: install requires at least one impl\n' >&2; exit 2; }

  system_packages=$(emit_extra_system_packages "$impls")
  if [ -n "$system_packages" ]; then
    printf 'ci/impl-toolchain.sh: installing system packages through mise: %s\n' "$(printf '%s\n' "$system_packages" | join_csv)"
    apt_config=$(mktemp)
    apt_config_path=/etc/apt/apt.conf.d/99kio-no-recommends-$$
    cleanup_apt_config() {
      rm -f "$apt_config"
      as_root rm -f "$apt_config_path"
    }
    cleanup_apt_config_and_exit() {
      st=$?
      cleanup_apt_config
      exit "$st"
    }
    trap cleanup_apt_config EXIT
    trap cleanup_apt_config_and_exit HUP INT TERM
    {
      printf '%s\n' 'APT::Install-Recommends "false";'
      printf '%s\n' 'APT::Install-Suggests "false";'
    } >"$apt_config"
    as_root install -m 0644 "$apt_config" "$apt_config_path"
    # shellcheck disable=SC2086
    MISE_EXPERIMENTAL=1 mise bootstrap packages apply --yes --update $system_packages
  else
    printf 'ci/impl-toolchain.sh: no extra system packages required\n'
  fi

  mise_tools=$(emit_extra_mise_tools "$impls")
  if [ -n "$mise_tools" ]; then
    printf 'ci/impl-toolchain.sh: installing mise tools: %s\n' "$(printf '%s\n' "$mise_tools" | join_csv)"
    mise trust "$REPO_ROOT/mise.toml"
    # shellcheck disable=SC2086
    mise install --locked -y $mise_tools
  else
    printf 'ci/impl-toolchain.sh: no extra mise tools required\n'
  fi
}

select_impls() {
  total_count=
  total_count_seen=0
  requested_extra_count=3
  extra_count_seen=0
  seed=${KIO_IMPL_TOOLCHAIN_SEED:-}

  while [ $# -gt 0 ]; do
    case "$1" in
      --count=*)
        [ "$total_count_seen" -eq 0 ] || { printf 'error: --count was provided more than once\n' >&2; exit 2; }
        total_count_seen=1
        total_count=${1#--count=}
        ;;
      --count)
        shift
        [ $# -gt 0 ] || { printf 'error: --count requires a value\n' >&2; exit 2; }
        [ "$total_count_seen" -eq 0 ] || { printf 'error: --count was provided more than once\n' >&2; exit 2; }
        total_count_seen=1
        total_count=$1
        ;;
      --extra-count=*)
        [ "$extra_count_seen" -eq 0 ] || { printf 'error: --extra-count was provided more than once\n' >&2; exit 2; }
        extra_count_seen=1
        requested_extra_count=${1#--extra-count=}
        ;;
      --extra-count)
        shift
        [ $# -gt 0 ] || { printf 'error: --extra-count requires a value\n' >&2; exit 2; }
        [ "$extra_count_seen" -eq 0 ] || { printf 'error: --extra-count was provided more than once\n' >&2; exit 2; }
        extra_count_seen=1
        requested_extra_count=$1
        ;;
      --seed=*) seed=${1#--seed=} ;;
      --seed)
        shift
        [ $# -gt 0 ] || { printf 'error: --seed requires a value\n' >&2; exit 2; }
        seed=$1
        ;;
      *) printf 'error: unknown select argument: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
  done

  if [ "$total_count_seen" -eq 1 ] && [ "$extra_count_seen" -eq 1 ]; then
    printf 'error: --count and --extra-count are mutually exclusive\n' >&2
    exit 2
  fi

  if [ "$total_count_seen" -eq 1 ]; then
    case "$total_count" in
      ''|*[!0-9]*) printf 'error: --count must be a positive integer\n' >&2; exit 2 ;;
      *) [ "$total_count" -ge 1 ] || { printf 'error: --count must be >= 1\n' >&2; exit 2; } ;;
    esac
  else
    case "$requested_extra_count" in
      ''|*[!0-9]*) printf 'error: --extra-count must be a non-negative integer\n' >&2; exit 2 ;;
    esac
  fi

  [ -n "$seed" ] || seed=$(date +%s)

  # shellcheck disable=SC2086
  set -- $CORE_IMPLS
  core_count=$#
  # shellcheck disable=SC2086
  set -- $EXTRA_IMPLS
  total_extra_count=$#

  if [ "$total_count_seen" -eq 1 ]; then
    extra_needed=0
    if [ "$total_count" -gt "$core_count" ]; then
      extra_needed=$((total_count - core_count))
      if [ "$extra_needed" -gt "$total_extra_count" ]; then
        extra_needed=$total_extra_count
      fi
    fi
  else
    extra_needed=$requested_extra_count
    if [ "$extra_needed" -gt "$total_extra_count" ]; then
      extra_needed=$total_extra_count
    fi
  fi

  if [ "$extra_needed" -gt 0 ]; then
    hash=$(printf '%s' "$seed" | cksum | awk '{ print $1 }')
    start=$((hash % total_extra_count))
    extras=$(
      # shellcheck disable=SC2086
      print_words $EXTRA_IMPLS |
        awk -v start="$start" -v need="$extra_needed" '
          { impls[NR] = $0 }
          END {
            for (i = 0; i < need && i < NR; i++) {
              print impls[((start + i) % NR) + 1]
            }
          }
        '
    )
  else
    extras=
  fi

  {
    print_core_impls
    printf '%s\n' "$extras"
  } | awk 'NF { print }' | join_csv
}

cmd=${1:-}
[ -n "$cmd" ] || { usage >&2; exit 2; }
shift

case "$cmd" in
  core-impls) print_core_impls | join_csv ;;
  extra-impls) print_extra_impls | join_csv ;;
  impls) print_all_impls ;;
  installed-impls) print_installed_impls ;;
  tooling-impls) print_tooling_impls "$@" ;;
  select) select_impls "$@" ;;
  core-mise-tools) print_core_mise_tools ;;
  report-mise-tools) print_report_mise_tools ;;
  install-report-tools) install_report_mise_tools ;;
  extra-mise-tools) emit_extra_mise_tools "$@" ;;
  extra-system-packages|extra-apt-packages) emit_extra_system_packages "$@" ;;
  install) install_impl_tools "$@" ;;
  -h|--help|help) usage ;;
  *) printf 'error: unknown command: %s\n' "$cmd" >&2; usage >&2; exit 2 ;;
esac
