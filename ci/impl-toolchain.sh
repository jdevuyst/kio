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

CORE_MISE_TOOLS="node rust tree-sitter actionlint zizmor sccache shellcheck python pipx yamllint ripgrep aqua:wasm-bindgen/wasm-pack npm:typescript npm:pyright pipx:mypy npm:markdownlint-cli2 github:mszostok/codeowners-validator[exe=codeowners-validator] http:kio-firefox"
REPORT_MISE_TOOLS="cargo:cargo-fuzz cargo:cargo-mutants cargo:cargo-llvm-cov cargo:cargo-audit"

usage() {
  cat <<EOF
Usage: sh ci/impl-toolchain.sh <command> [args]

Commands:
  toolchain-bootstrap
      Install only the verified pinned mise manager, without language tools.
  toolchain-plan|toolchain-install|toolchain-verify <languages> [--environment ID]
      Inspect, install, or verify exact target-language environments.
  toolchain-exec <languages> [--environment ID] [--core|--reports|--helpers IDs] -- <command> [args...]
      Verify and execute in the selected environment without installation.
  toolchain-env|toolchain-github-env <languages> [--environment ID] [--core|--reports]
      Verify and export the selection for a shell or later GitHub steps.
  toolchain-environments
      List native mise environments; development uses the root configuration.
  supported-languages|portability-languages
      Print the corresponding development setup set for this operating system.
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
  core-helper-tools
      Print general development tools, excluding the shared language tools.
  install-core
      Install core language environments and general development helpers.
  install-browser-system-packages [--dry-run]
      Install Firefox's Linux runtime libraries through mise bootstrap.
      macOS and Windows need no extra browser runtime package install.
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
      Install exact owned environments for the implementation list.
EOF
}

toolchain_languages() {
  case "${1:-supported}" in
    all) printf '%s\n' js ts python rust go java swift haskell ;;
    supported)
      case "$(uname -s)" in
        Linux) toolchain_languages all ;;
        Darwin) printf '%s\n' js ts rust swift ;;
        MINGW*|MSYS*|CYGWIN*) printf '%s\n' rust ;;
        *) printf 'error: unsupported setup platform\n' >&2; return 2 ;;
      esac ;;
    portability)
      case "$(uname -s)" in
        Linux) printf '%s\n' js ts rust ;;
        *) toolchain_languages supported ;;
      esac ;;
  esac
}

toolchain_tool_names() {
  WORDS=
  for language in $(normalize_impl_args "$@" | sed 's/^kio@//'); do
    case "$language" in
      js) add_word node ;;
      ts) add_word node; add_word npm:typescript ;;
      python) add_word python; add_word node; add_word npm:pyright ;;
      rust) add_word rust ;;
      go) add_word go ;;
      java) add_word java ;;
      swift) add_word swift ;;
      haskell) add_word ghcup ;;
      *) printf 'error: unknown target language: %s\n' "$language" >&2; return 2 ;;
    esac
  done
  printf '%s\n' "$WORDS"
}

warn_toolchain_languages() {
  supported=$(toolchain_languages supported)
  for language in $(normalize_impl_args "$@" | sed 's/^kio@//'); do
    if ! printf '%s\n' "$supported" | grep -qx "$language"; then
      printf 'warning: %s setup is best-effort on this OS; native prerequisites may require separate installation\n' "$language" >&2
    fi
  done
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

print_core_helper_tools() {
  print_core_mise_tools | while IFS= read -r tool; do
    case "$tool" in node|rust|python|npm:typescript|npm:pyright) ;; *) printf '%s\n' "$tool" ;; esac
  done
}

install_core_tools() {
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-install rust; return ;;
  esac
  sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-install js,ts,python,rust
  # shellcheck disable=SC2046
  sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-general-install js,ts,python,rust -- $(print_core_helper_tools)
  sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-exec js,ts,python,rust --core -- \
    sh "$SCRIPT_DIR/impl-toolchain.sh" install-browser-system-packages
}

print_report_mise_tools() {
  # shellcheck disable=SC2086
  print_words $REPORT_MISE_TOOLS
}

install_report_mise_tools() {
  sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-install rust
  sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-install rust --environment fuzz
  # shellcheck disable=SC2086
  sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-general-install rust --environment optional -- $REPORT_MISE_TOOLS
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
          binutils gnupg libc6-dev libcurl4-openssl-dev libedit2 libgcc-13-dev \
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

install_browser_system_packages() {
  case "$*" in
    ''|--dry-run) ;;
    *) printf 'error: install-browser-system-packages accepts only --dry-run\n' >&2; exit 2 ;;
  esac
  case "$(uname -s)" in
    Linux) ;;
    *) printf 'ci/impl-toolchain.sh: no extra browser runtime packages required\n'; return ;;
  esac
  if [ ! -r /etc/os-release ]; then
    printf 'error: browser runtime bootstrap needs Debian/Ubuntu /etc/os-release\n' >&2
    exit 2
  fi
  # This package route is Linux-specific; the browser archive itself is mise-owned on every OS.
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-}:${VERSION_ID:-}" in
    ubuntu:22.04|debian:12) t64= ;;
    ubuntu:24.04|ubuntu:26.04|debian:13) t64=t64 ;;
    *) printf 'error: browser runtime bootstrap has no package mapping for %s %s; use a supported Debian/Ubuntu runtime or the devcontainer\n' "${ID:-unknown}" "${VERSION_ID:-unknown}" >&2; exit 2 ;;
  esac
  MISE_EXPERIMENTAL=1 mise bootstrap packages apply --yes --update "$@" \
    "apt:libasound2$t64" "apt:libatk1.0-0$t64" "apt:libglib2.0-0$t64" "apt:libgtk-3-0$t64" \
    apt:libcairo-gobject2 apt:libcairo2 apt:libdbus-1-3 apt:libdbus-glib-1-2 \
    apt:libfontconfig1 apt:libfreetype6 apt:libgdk-pixbuf-2.0-0 \
    apt:libpango-1.0-0 apt:libpangocairo-1.0-0 apt:libx11-6 apt:libx11-xcb1 \
    apt:libxcb-shm0 apt:libxcb1 apt:libxcomposite1 apt:libxcursor1 apt:libxdamage1 \
    apt:libxext6 apt:libxfixes3 apt:libxi6 apt:libxrandr2 apt:libxrender1 apt:libxtst6 \
    apt:fonts-liberation
}

install_impl_tools() {
  impls=$(normalize_impl_args "$@")
  [ -n "$impls" ] || { printf 'error: install requires at least one impl\n' >&2; exit 2; }

  for impl in $impls; do validate_impl "$impl"; done
  languages=$(printf '%s\n' "$impls" | sed 's/^kio@//' | join_csv)
  sh "$SCRIPT_DIR/impl-toolchain.sh" toolchain-install "$languages"
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
  toolchain-bootstrap|toolchain-plan|toolchain-install|toolchain-verify|toolchain-exec|toolchain-env|toolchain-key|toolchain-github-env|toolchain-general-install)
    # shellcheck disable=SC1091
    . "$REPO_ROOT/tools/toolchains/provision.sh"
    toolchain_dispatch "$cmd" "$@" ;;
  toolchain-environments)
    printf '%s\n' development
    for config in "$REPO_ROOT"/mise.*.toml; do
      [ -f "$config" ] || continue
      environment=${config##*/mise.}
      printf '%s\n' "${environment%.toml}"
    done ;;
  supported-languages) toolchain_languages supported ;;
  portability-languages) toolchain_languages portability ;;
  core-impls) print_core_impls | join_csv ;;
  extra-impls) print_extra_impls | join_csv ;;
  impls) print_all_impls ;;
  installed-impls) print_installed_impls ;;
  tooling-impls) print_tooling_impls "$@" ;;
  select) select_impls "$@" ;;
  core-mise-tools) print_core_mise_tools ;;
  core-helper-tools) print_core_helper_tools ;;
  install-core) install_core_tools ;;
  install-browser-system-packages) install_browser_system_packages "$@" ;;
  report-mise-tools) print_report_mise_tools ;;
  install-report-tools) install_report_mise_tools ;;
  extra-mise-tools) emit_extra_mise_tools "$@" ;;
  extra-system-packages|extra-apt-packages) emit_extra_system_packages "$@" ;;
  install) install_impl_tools "$@" ;;
  -h|--help|help) usage ;;
  *) printf 'error: unknown command: %s\n' "$cmd" >&2; usage >&2; exit 2 ;;
esac
