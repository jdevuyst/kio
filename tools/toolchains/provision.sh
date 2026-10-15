#!/bin/sh
# Sourced by ci/impl-toolchain.sh, which owns language/dependency selection.
# The helper lookup intentionally isolates general mise roots in a subshell.
# shellcheck disable=SC2030,SC2031

toolchain_fail() { printf 'error: %s\n' "$*" >&2; exit 2; }

toolchain_hash() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@" | awk '{print $1}';
  else shasum -a 256 "$@" | awk '{print $1}'; fi
}

toolchain_native_path() {
  case "$TC_PLATFORM" in windows-*) cygpath -m "$1" ;; *) printf '%s\n' "$1" ;; esac
}

toolchain_shell_path() {
  case "$TC_PLATFORM" in windows-*) cygpath -u "$1" ;; *) printf '%s\n' "$1" ;; esac
}

toolchain_enable_names() {
  # Native enable filters match tool identities, not option-bearing requests.
  printf '%s\n' "$*" | sed 's/\[[^]]*\]//g' | tr ' \n' ','
}

toolchain_real_path() {
  tc_resolved=$(toolchain_native_path "$1")
  tc_links=0
  while :; do
    tc_parent=$(CDPATH='' cd -- "$(dirname -- "$tc_resolved")" && pwd -P) || return 1
    tc_resolved="$tc_parent/${tc_resolved##*/}"
    [ -L "$tc_resolved" ] || break
    tc_links=$((tc_links + 1))
    [ "$tc_links" -lt 40 ] || return 1
    tc_link=$(readlink "$tc_resolved") || return 1
    case "$tc_link" in /*|[A-Za-z]:/*) tc_resolved=$tc_link ;; *) tc_resolved="$tc_parent/$tc_link" ;; esac
  done
  toolchain_native_path "$tc_resolved"
}

toolchain_platform() {
  case "$(uname -s)" in
    Linux) tc_os=linux ;;
    Darwin) tc_os=macos ;;
    MINGW*|MSYS*|CYGWIN*) tc_os=windows ;;
    *) toolchain_fail 'setup requires Linux, macOS, or Git for Windows' ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64|AMD64) tc_arch=x64 ;;
    aarch64|arm64) tc_arch=arm64 ;;
    *) toolchain_fail 'setup has no verified manager bootstrap for this architecture' ;;
  esac
  TC_PLATFORM=$tc_os-$tc_arch
}

toolchain_digest() {
  {
    printf '%s\n' "$TC_PLATFORM" "$TC_ENVIRONMENT" "$TC_ROOT"
    printf '%s\n' "${ImageOS:-}"
    if [ "$TC_PLATFORM" = linux-x64 ] || [ "$TC_PLATFORM" = linux-arm64 ]; then
      if [ -r /etc/os-release ]; then
        ( . /etc/os-release; printf '%s\n' "$ID" "$VERSION_ID" )
      fi
      if command -v ldd >/dev/null 2>&1; then ldd --version 2>&1 | head -1; fi
    fi
    for tc_file in "$REPO_ROOT"/mise*.toml "$REPO_ROOT"/mise*.lock \
        "$REPO_ROOT"/tools/toolchains/* "$REPO_ROOT/ci/impl-toolchain.sh"; do
      [ -f "$tc_file" ] || continue
      printf '%s ' "${tc_file#"$REPO_ROOT/"}"
      toolchain_hash "$tc_file"
    done
  } | toolchain_hash
}

toolchain_mise() {
  export MISE_SELF_UPDATE_AVAILABLE=0
  tc_expected=$(tr -d '\r\n' <"$REPO_ROOT/tools/toolchains/mise.version")
  TC_MISE="$TC_ROOT/bootstrap/$tc_expected/mise"
  case "$TC_PLATFORM" in windows-*) TC_MISE=$TC_MISE.exe ;; esac
  if [ ! -x "$TC_MISE" ]; then
    case "$TC_OPERATION" in
      toolchain-bootstrap|toolchain-install|toolchain-general-install)
        TC_MISE=$(sh "$REPO_ROOT/tools/toolchains/bootstrap.sh" "$TC_ROOT" "$TC_PLATFORM") ;;
      toolchain-plan|toolchain-key)
        TC_MISE=$(command -v mise) || toolchain_fail 'install mise to inspect the native configuration; the installer can bootstrap it'
        ;;
      *) toolchain_fail 'selected environment is not installed; run ci/install-target-language-toolchains.sh first' ;;
    esac
  fi
  case "$("$TC_MISE" --version)" in
    "$tc_expected "*) ;;
    *) toolchain_fail "this provisioning recipe requires mise $tc_expected" ;;
  esac
  tc_hash=$(awk -v p="$TC_PLATFORM" '$1 == p {print $2}' "$REPO_ROOT/tools/toolchains/mise.sha256")
  [ "$(toolchain_hash "$TC_MISE")" = "$tc_hash" ] || toolchain_fail 'mise executable checksum mismatch'
}

toolchain_scalar() {
  "$TC_MISE" config get "$1" --file "$2"
}

toolchain_tool_config() {
  tc_tool=$1
  tc_config="$REPO_ROOT/mise.toml"
  if [ "$TC_ENVIRONMENT" != development ] && \
      toolchain_scalar "tools.$tc_tool" "$REPO_ROOT/mise.$TC_ENVIRONMENT.toml" >/dev/null 2>&1; then
    tc_config="$REPO_ROOT/mise.$TC_ENVIRONMENT.toml"
  elif ! toolchain_scalar "tools.$tc_tool" "$tc_config" >/dev/null 2>&1 && \
      toolchain_scalar "tools.$tc_tool" "$REPO_ROOT/mise.optional.toml" >/dev/null 2>&1; then
    tc_config="$REPO_ROOT/mise.optional.toml"
  fi
  printf '%s\n' "$tc_config"
}

toolchain_version() {
  tc_tool=$1
  tc_config=$(toolchain_tool_config "$tc_tool")
  toolchain_scalar "tools.$tc_tool.version" "$tc_config" 2>/dev/null || toolchain_scalar "tools.$tc_tool" "$tc_config"
}

toolchain_lock() {
  mkdir -p "$TC_ROOT"
  tc_attempt=0
  while ! mkdir "$TC_ROOT/setup.lock" 2>/dev/null; do
    tc_attempt=$((tc_attempt + 1))
    if [ "$tc_attempt" -eq 1 ]; then printf 'toolchain: waiting for another setup of %s\n' "$TC_ROOT" >&2; fi
    [ "$tc_attempt" -lt 3600 ] || toolchain_fail "another setup owns $TC_ROOT/setup.lock; remove it only after verifying that store is idle"
    sleep 1
  done
  TC_LOCK="$TC_ROOT/setup.lock"
  printf '%s\n' "$$" >"$TC_LOCK/pid"
  trap 'rm -f "$TC_LOCK/pid"; rmdir "$TC_LOCK"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
}

toolchain_snapshot() {
  TC_SNAPSHOT="$TC_ROOT/config/$TC_DIGEST"
  if [ ! -f "$TC_SNAPSHOT/mise.toml" ]; then
    case "$TC_OPERATION" in toolchain-install|toolchain-general-install) ;; *) toolchain_fail 'configuration changed; install the selected environment before execution' ;; esac
    mkdir -p "$TC_ROOT/config"
    tc_staging=$(mktemp -d "$TC_ROOT/config/staging.XXXXXX")
    cp "$REPO_ROOT/mise.toml" "$REPO_ROOT/mise.lock" "$tc_staging/"
    for tc_config in "$REPO_ROOT"/mise.*.toml "$REPO_ROOT"/mise.*.lock; do
      [ -f "$tc_config" ] || continue
      cp "$tc_config" "$tc_staging/"
    done
    if [ "$TC_ENVIRONMENT" != development ]; then
      mkdir "$tc_staging/selected"
      cp "$REPO_ROOT/mise.$TC_ENVIRONMENT.toml" "$tc_staging/selected/mise.toml"
      cp "$REPO_ROOT/mise.$TC_ENVIRONMENT.lock" "$tc_staging/selected/mise.lock"
    fi
    mkdir "$tc_staging/optional"
    cp "$REPO_ROOT/mise.optional.toml" "$tc_staging/optional/mise.toml"
    cp "$REPO_ROOT/mise.optional.lock" "$tc_staging/optional/mise.lock"
    : >"$tc_staging/empty.toml"
    mv "$tc_staging" "$TC_SNAPSHOT"
  fi
}

toolchain_controlled_environment() {
  # Native install callbacks (including npm's reshim hook) invoke mise by name.
  PATH="$(toolchain_shell_path "${TC_MISE%/*}"):$PATH"
  export PATH
  export MISE_DATA_DIR="$TC_ROOT/mise/data" MISE_CACHE_DIR="$TC_ROOT/mise/cache"
  export MISE_STATE_DIR="$TC_ROOT/mise/state" MISE_CONFIG_DIR="$TC_ROOT/mise/config"
  export MISE_SYSTEM_CONFIG_DIR="$TC_ROOT/mise/system-config" MISE_SYSTEM_DATA_DIR="$TC_ROOT/mise/system-data"
  export MISE_TMP_DIR="$TC_ROOT/mise/tmp"
  export MISE_GLOBAL_CONFIG_ROOT="$TC_SNAPSHOT" MISE_USE_TOML=1
  export MISE_SYSTEM_CONFIG_FILE="$TC_SNAPSHOT/mise.toml"
  case "$TC_ENVIRONMENT" in
    development) MISE_GLOBAL_CONFIG_FILE="$TC_SNAPSHOT/empty.toml" ;;
    *) MISE_GLOBAL_CONFIG_FILE="$TC_SNAPSHOT/selected/mise.toml" ;;
  esac
  export MISE_GLOBAL_CONFIG_FILE
  export MISE_OVERRIDE_CONFIG_FILENAMES="/.kio-disabled-$TC_DIGEST/config.toml"
  export MISE_OVERRIDE_TOOL_VERSIONS_FILENAMES=none
  export MISE_CEILING_PATHS="$TC_SNAPSHOT" MISE_IDIOMATIC_VERSION_FILE=0
  export MISE_AUTO_ENV=0 MISE_NO_CONFIG=0 MISE_NO_ENV=1 MISE_NO_HOOKS=1
  export MISE_AUTO_INSTALL=0 MISE_EXEC_AUTO_INSTALL=0 MISE_SELF_UPDATE_AVAILABLE=0
  export MISE_TRUSTED_CONFIG_PATHS="$TC_SNAPSHOT" MISE_COLOR=0
  export MISE_ENABLE_TOOLS
  MISE_ENABLE_TOOLS=$(printf '%s\n' "$TC_TOOLS" | tr ' ' ',')
  # An explicit selected overlay avoids native environment discovery after cd.
  export MISE_ENV=
  unset MISE_CONFIG_FILE MISE_SHARED_INSTALL_DIRS
  # Runtime version overrides replace option-bearing requests in pinned mise.
  TC_UNSET_VERSIONS=$(env | sed -n 's/^\(MISE_[A-Za-z0-9_]*_VERSION\)=.*/\1/p')
  for tc_name in $TC_UNSET_VERSIONS; do unset "$tc_name"; done
  export RUSTUP_HOME="$TC_ROOT/rustup" RUSTUP_AUTO_INSTALL=0
  export GHCUP_INSTALL_BASE_PREFIX="$TC_ROOT/ghcup" CABAL_DIR="$TC_ROOT/cabal"
  unset GHCUP_USE_XDG_DIRS
  export npm_config_cache="$TC_ROOT/npm/cache"
  export GOENV=off GOTOOLCHAIN=local
}

toolchain_rustup_install() {
  tc_manifest="$REPO_ROOT/tools/toolchains/rustup.toml"
  tc_version=$(toolchain_scalar version "$tc_manifest")
  tc_triple=$(toolchain_scalar "platform.$TC_PLATFORM.triple" "$tc_manifest")
  tc_hash=$(toolchain_scalar "platform.$TC_PLATFORM.sha256" "$tc_manifest")
  export CARGO_HOME="$TC_ROOT/cargo-manager"
  tc_rustup="$CARGO_HOME/bin/rustup"
  case "$TC_PLATFORM" in windows-*) tc_suffix=.exe ;; *) tc_suffix= ;; esac
  if [ ! -f "$tc_rustup$tc_suffix" ] || ! "$tc_rustup" --version 2>/dev/null | head -1 | grep -q "^rustup $tc_version "; then
    mkdir -p "$TC_ROOT/downloads"
    tc_init="$TC_ROOT/downloads/rustup-init-$tc_version-$TC_PLATFORM$tc_suffix"
    if [ ! -f "$tc_init" ] || [ "$(toolchain_hash "$tc_init")" != "$tc_hash" ]; then
      curl --fail --location --silent --show-error --retry 2 \
        "https://static.rust-lang.org/rustup/archive/$tc_version/$tc_triple/rustup-init$tc_suffix" >"$tc_init.partial"
      [ "$(toolchain_hash "$tc_init.partial")" = "$tc_hash" ] || toolchain_fail 'Rustup initializer checksum mismatch'
      chmod +x "$tc_init.partial"
      mv "$tc_init.partial" "$tc_init"
    fi
    "$tc_init" -y --no-modify-path --default-toolchain none --profile minimal >&2
  fi
  "$tc_rustup" set auto-self-update disable >&2
  PATH="$(toolchain_shell_path "$CARGO_HOME/bin"):$PATH"
  export PATH
}

toolchain_install_selected() {
  unset MISE_OFFLINE
  if [ "${TC_PLATFORM%%-*}" = linux ]; then
    tc_impls=$(printf '%s' "$TC_LANGUAGES" | tr ', ' '\n' | sed '/^$/d; s/^kio@//; s/^/kio@/')
    tc_packages=$(emit_extra_system_packages "$tc_impls")
    if [ -n "$tc_packages" ]; then
      # shellcheck disable=SC2086
      MISE_EXPERIMENTAL=1 "$TC_MISE" bootstrap packages apply --yes --update $tc_packages >&2
    fi
  fi
  case " $TC_TOOLS " in *' rust '*) toolchain_rustup_install ;; esac
  set --
  for tc_tool in $TC_TOOLS; do set -- "$@" "$tc_tool@$(toolchain_version "$tc_tool")"; done
  case " $TC_TOOLS " in
    *' swift '*)
      case "$TC_PLATFORM" in
        linux-*)
          command -v gpg >/dev/null 2>&1 || toolchain_fail 'Linux Swift setup requires GnuPG for native upstream signature verification'
          mkdir -p "$TC_ROOT/swift/gnupg"
          chmod 700 "$TC_ROOT/swift/gnupg"
          GNUPGHOME="$TC_ROOT/swift/gnupg" MISE_SWIFT_GPG_VERIFY=1 "$TC_MISE" install --locked -y "$@" >&2 ;;
        *) "$TC_MISE" install --locked -y "$@" >&2 ;;
      esac
      ;;
    *) "$TC_MISE" install --locked -y "$@" >&2 ;;
  esac
  case " $TC_TOOLS " in
    *' ghcup '*)
      # shellcheck disable=SC1091
      . "$REPO_ROOT/tools/toolchains/ghcup.sh"
      toolchain_ghcup_install ;;
  esac
}

toolchain_verify_selected() {
  export MISE_OFFLINE=1
  TC_PATH=
  for tc_tool in $TC_TOOLS; do
    tc_version=$(toolchain_version "$tc_tool")
    case "$tc_tool" in
      node) tc_executables=node ;;
      npm:typescript) tc_executables=tsc ;;
      npm:pyright) tc_executables=pyright ;;
      python) case "$TC_PLATFORM" in windows-*) tc_executables=python ;; *) tc_executables=python3 ;; esac ;;
      rust)
        tc_rustup="$TC_ROOT/cargo-manager/bin/rustup"
        case "$TC_PLATFORM" in windows-*) tc_rustup=$tc_rustup.exe ;; esac
        [ -x "$tc_rustup" ] || [ -x "$tc_rustup.exe" ] || toolchain_fail 'owned Rustup manager is missing'
        tc_expected=$(toolchain_scalar version "$REPO_ROOT/tools/toolchains/rustup.toml")
        tc_manager_hash=$(toolchain_scalar "platform.$TC_PLATFORM.sha256" "$REPO_ROOT/tools/toolchains/rustup.toml")
        [ "$(toolchain_hash "$tc_rustup")" = "$tc_manager_hash" ] || toolchain_fail 'owned Rustup manager checksum mismatch'
        "$tc_rustup" --version 2>/dev/null | head -1 | grep -q "^rustup $tc_expected " || toolchain_fail 'owned Rustup manager version mismatch'
        tc_rustc=$("$tc_rustup" which --toolchain "$tc_version" rustc) || toolchain_fail "Rust $tc_version is missing"
        tc_rustc=$(toolchain_real_path "$tc_rustc")
        case "$tc_rustc" in "$TC_ROOT/rustup/toolchains/"*/bin/rustc*) ;; *) toolchain_fail 'Rust compiler escaped its owned root' ;; esac
        tc_observed=$("$tc_rustc" --version)
        case "$tc_version" in
          nightly-*) printf '%s\n' "$tc_observed" | grep -q '^rustc .*nightly ' || toolchain_fail 'Rust nightly version mismatch' ;;
          *) printf '%s\n' "$tc_observed" | grep -q "^rustc $tc_version " || toolchain_fail 'Rust version mismatch' ;;
        esac
        tc_bin=${tc_rustc%/*}
        export RUSTC="$tc_rustc"
        RUSTDOC="$tc_bin/rustdoc"
        case "$TC_PLATFORM" in windows-*) RUSTDOC=$RUSTDOC.exe ;; esac
        [ -x "$RUSTDOC" ] || toolchain_fail 'selected rustdoc is missing'
        tc_rustdoc_real=$(toolchain_real_path "$RUSTDOC")
        case "$tc_rustdoc_real" in "${tc_bin%/bin}/"*) ;; *) toolchain_fail 'rustdoc escaped its exact toolchain' ;; esac
        export RUSTDOC
        tc_cargo="$tc_bin/cargo"
        case "$TC_PLATFORM" in windows-*) tc_cargo=$tc_cargo.exe ;; esac
        [ -x "$tc_cargo" ] || toolchain_fail 'selected Cargo is missing'
        tc_cargo_real=$(toolchain_real_path "$tc_cargo")
        case "$tc_cargo_real" in "${tc_bin%/bin}/"*) ;; *) toolchain_fail 'Cargo escaped its exact toolchain' ;; esac
        TC_PATH="$(toolchain_shell_path "$tc_bin")${TC_PATH:+:$TC_PATH}"
        TC_RUST_VERSION=$tc_version
        tc_config=$(toolchain_tool_config rust)
        tc_targets=$(toolchain_scalar tools.rust.targets "$tc_config" 2>/dev/null || true)
        if [ -n "$tc_targets" ]; then
          tc_installed=$("$tc_rustup" target list --installed --toolchain "$tc_version")
          for tc_target in $(printf '%s' "$tc_targets" | tr ',' ' '); do
            printf '%s\n' "$tc_installed" | grep -qx "$tc_target" || toolchain_fail "missing Rust target $tc_target"
          done
        fi
        tc_components=$(toolchain_scalar tools.rust.components "$tc_config" 2>/dev/null || true)
        for tc_component in $(printf '%s' "$tc_components" | tr ',' ' '); do
          "$tc_rustup" component list --installed --toolchain "$tc_version" | grep -q "^$tc_component-" || toolchain_fail "missing Rust component $tc_component"
        done
        printf 'toolchain: rust %s %s\n' "$tc_version" "$tc_rustc" >&2
        printf 'toolchain-verified: rust observed=%s\n' "$tc_observed" >&2
        continue ;;
      go) tc_executables=go ;;
      java) tc_executables='java javac' ;;
      swift) tc_executables='swift swiftc' ;;
      ghcup) tc_executables=ghcup ;;
      *) toolchain_fail "no executable contract for $tc_tool" ;;
    esac
    tc_install=$("$TC_MISE" where "$tc_tool@$tc_version") || toolchain_fail "missing $tc_tool@$tc_version"
    tc_install=$(toolchain_native_path "$tc_install")
    tc_install=$(CDPATH='' cd -- "$tc_install" && pwd -P)
    tc_install=$(toolchain_native_path "$tc_install")
    case "$tc_install" in "$TC_ROOT/mise/data/installs/"*) ;; *) toolchain_fail "unexpected install origin for $tc_tool" ;; esac
    for tc_exe in $tc_executables; do
      tc_path=$("$TC_MISE" which "$tc_exe" --tool "$tc_tool@$tc_version") || toolchain_fail "missing $tc_tool@$tc_version executable $tc_exe"
      tc_path=$(toolchain_native_path "$tc_path")
      tc_real_path=$(toolchain_real_path "$tc_path")
      case "$tc_real_path" in "$tc_install/"*) ;; *) toolchain_fail "$tc_exe escaped its exact installation" ;; esac
      [ -x "$tc_path" ] || toolchain_fail "$tc_exe is incomplete"
      TC_PATH="$(toolchain_shell_path "${tc_path%/*}")${TC_PATH:+:$TC_PATH}"
      printf 'toolchain: %s %s %s\n' "$tc_tool" "$tc_version" "$tc_path" >&2
    done
    case "$tc_tool" in
      node) tc_observed=$("$tc_path" --version); [ "$tc_observed" = "v$tc_version" ] || toolchain_fail 'Node version mismatch' ;;
      python)
        unset PYTHONHOME
        TC_PYTHON_SELECTED=1
        tc_observed=$("$tc_path" --version); [ "$tc_observed" = "Python $tc_version" ] || toolchain_fail 'Python version mismatch' ;;
      npm:typescript) tc_observed=$(PATH="$TC_PATH:$PATH" "$tc_path" --version); [ "$tc_observed" = "Version $tc_version" ] || toolchain_fail 'TypeScript version mismatch' ;;
      npm:pyright) tc_observed=$(PATH="$TC_PATH:$PATH" "$tc_path" --version); [ "$tc_observed" = "pyright $tc_version" ] || toolchain_fail 'Pyright version mismatch' ;;
      go)
        export GOROOT="$tc_install"
        tc_observed=$("$tc_path" version); printf '%s\n' "$tc_observed" | grep -q " go$tc_version " || toolchain_fail 'Go version mismatch' ;;
      java)
        export JAVA_HOME="$tc_install"
        tc_config=$(toolchain_tool_config java)
        tc_java_url=$(awk -v section="[tools.java.\"platforms.$TC_PLATFORM\"]" '
          /^\[/ { active = ($0 == section) }
          active && /^url = / { sub(/^url = "/, ""); sub(/"$/, ""); print; exit }
        ' "${tc_config%.toml}.lock")
        tc_java=$(printf '%s' "$tc_java_url" | sed -n 's@.*/releases/download/jdk-\([^/]*\)/.*@\1@p' | sed 's/%2B/+/g')
        [ -n "$tc_java" ] || toolchain_fail 'the Java lock has no recognized exact Temurin artifact'
        tc_java_path=$("$TC_MISE" which java --tool "java@$tc_version")
        tc_observed=$("$tc_java_path" --version 2>&1)
        printf '%s\n' "$tc_observed" | grep -Fq "$tc_java" || toolchain_fail 'Java version mismatch'
        tc_observed=$(printf '%s' "$tc_observed" | tr '\n' ';') ;;
      swift)
        tc_observed=$("$tc_path" --version)
        tc_swift=$(printf '%s\n' "$tc_observed" | sed -n 's/.*Swift version \([0-9][0-9.]*\).*/\1/p' | head -1)
        case "$tc_swift" in *.*.*) ;; *.*) tc_swift=$tc_swift.0 ;; esac
        [ "$tc_swift" = "$tc_version" ] || toolchain_fail 'Swift version mismatch'
        tc_observed=$(printf '%s' "$tc_observed" | tr '\n' ';') ;;
      ghcup) tc_observed=$("$tc_path" --numeric-version); [ "$tc_observed" = "$tc_version" ] || toolchain_fail 'GHCup version mismatch' ;;
    esac
    printf 'toolchain-verified: %s observed=%s\n' "$tc_tool" "$tc_observed" >&2
  done
  case " $TC_TOOLS " in *' ghcup '*)
    # shellcheck disable=SC1091
    . "$REPO_ROOT/tools/toolchains/ghcup.sh"
    toolchain_ghcup_verify ;;
  esac
  export PATH="$TC_PATH:$PATH"
  case " $TC_TOOLS " in *' swift '*)
    if [ "$TC_PLATFORM" = macos-arm64 ] || [ "$TC_PLATFORM" = macos-x64 ]; then
      SDKROOT=${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}
      export SDKROOT
    fi ;;
  esac
}

toolchain_verify_helpers() {
  tc_general_root=${TC_GENERAL_DATA:-${XDG_DATA_HOME:-$HOME/.local/share}/mise}
  tc_helper_tools=$TC_HELPERS
  tc_helper_paths=$(
    if [ -n "$TC_GENERAL_DATA" ]; then export MISE_DATA_DIR="$TC_GENERAL_DATA"; else unset MISE_DATA_DIR; fi
    if [ -n "$TC_GENERAL_CACHE" ]; then export MISE_CACHE_DIR="$TC_GENERAL_CACHE"; else unset MISE_CACHE_DIR; fi
    if [ -n "$TC_GENERAL_STATE" ]; then export MISE_STATE_DIR="$TC_GENERAL_STATE"; else unset MISE_STATE_DIR; fi
    export MISE_SHARED_INSTALL_DIRS="$TC_ROOT/mise/data/installs"
    # Optional helpers retain their native options/locks in a separate root.
    export MISE_GLOBAL_CONFIG_FILE="$TC_SNAPSHOT/optional/mise.toml"
    MISE_ENABLE_TOOLS=$(toolchain_enable_names "$TC_TOOLS" "$tc_helper_tools")
    export MISE_ENABLE_TOOLS
    set --
    for tc_tool in $tc_helper_tools; do
      tc_version=$(toolchain_version "$tc_tool")
      tc_install=$("$TC_MISE" where "$tc_tool@$tc_version") || toolchain_fail "missing helper $tc_tool@$tc_version; run install-core or install-report-tools"
      [ -d "$tc_install" ] || toolchain_fail "incomplete core helper $tc_tool"
      tc_install=$(toolchain_real_path "$tc_install")
      case "$tc_tool" in
        (ripgrep) tc_exe=rg ;;
        (http:kio-firefox) tc_exe=firefox ;;
        (aqua:axodotdev/cargo-dist) tc_exe=dist ;;
        (npm:@devcontainers/cli) tc_exe=devcontainer ;;
        (github:mszostok/codeowners-validator*) tc_exe=codeowners-validator ;;
        (*) tc_exe=${tc_tool##*:}; tc_exe=${tc_exe##*/} ;;
      esac
      tc_executable=$("$TC_MISE" which "$tc_exe" --tool "$tc_tool@$tc_version") || toolchain_fail "missing helper executable $tc_exe"
      tc_real_executable=$(toolchain_real_path "$tc_executable")
      case "$tc_real_executable" in ("$tc_install/"*) ;; (*) toolchain_fail "helper $tc_exe escaped its exact installation" ;; esac
      [ -x "$tc_executable" ] || toolchain_fail "incomplete helper executable $tc_exe"
      printf 'toolchain-helper: %s %s %s\n' "$tc_tool" "$tc_version" "$tc_executable" >&2
      set -- "$@" "$tc_tool@$tc_version"
    done
    "$TC_MISE" bin-paths "$@"
  ) || toolchain_fail 'core helper verification failed'
  while IFS= read -r tc_helper_path; do
    [ -d "$tc_helper_path" ] || toolchain_fail "incomplete helper path: $tc_helper_path"
    TC_PATH="$TC_PATH:$(toolchain_shell_path "$tc_helper_path")"
  done <<EOF
$tc_helper_paths
EOF
  export MISE_SHARED_INSTALL_DIRS="$tc_general_root/installs"
  MISE_ENABLE_TOOLS=$(toolchain_enable_names "$TC_TOOLS" "$tc_helper_tools")
  export MISE_ENABLE_TOOLS
}

toolchain_export_environment() {
  for tc_name in PATH KIO_TOOLCHAINS_ROOT KIO_TOOLCHAIN_ENVIRONMENT KIO_TOOLCHAIN_IDENTITY KIO_GENERAL_MISE_DATA_DIR \
      MISE_DATA_DIR MISE_CACHE_DIR MISE_STATE_DIR MISE_CONFIG_DIR MISE_SYSTEM_CONFIG_DIR \
      MISE_SYSTEM_DATA_DIR MISE_TMP_DIR MISE_GLOBAL_CONFIG_FILE MISE_GLOBAL_CONFIG_ROOT \
      MISE_SYSTEM_CONFIG_FILE MISE_OVERRIDE_CONFIG_FILENAMES MISE_OVERRIDE_TOOL_VERSIONS_FILENAMES \
      MISE_CEILING_PATHS MISE_IDIOMATIC_VERSION_FILE MISE_AUTO_ENV MISE_NO_CONFIG MISE_NO_ENV \
      MISE_NO_HOOKS MISE_AUTO_INSTALL MISE_EXEC_AUTO_INSTALL MISE_SELF_UPDATE_AVAILABLE \
      MISE_TRUSTED_CONFIG_PATHS MISE_COLOR MISE_ENABLE_TOOLS MISE_ENV MISE_OFFLINE \
      MISE_SHARED_INSTALL_DIRS MISE_USE_TOML RUSTUP_HOME RUSTUP_AUTO_INSTALL RUSTUP_TOOLCHAIN RUSTC RUSTDOC \
      GHCUP_INSTALL_BASE_PREFIX CABAL_DIR GOENV GOTOOLCHAIN GOROOT JAVA_HOME PYTHONHOME SDKROOT npm_config_cache; do
    tc_value=$(printenv "$tc_name" 2>/dev/null) || continue
    case "$TC_OPERATION" in
      toolchain-github-env)
        [ -n "${GITHUB_ENV:-}" ] || toolchain_fail 'GITHUB_ENV is required'
        if [ "$tc_name" = PATH ]; then
          case "$TC_PLATFORM" in windows-*) tc_value=$(cygpath -wp "$tc_value") ;; esac
        fi
        case "$tc_value" in *'
'*) toolchain_fail "newline in exported $tc_name" ;; esac
        printf '%s=%s\n' "$tc_name" "$tc_value" >>"$GITHUB_ENV" ;;
      *)
        tc_quoted=$(printf '%s' "$tc_value" | sed "s/'/'\\\\''/g")
        printf "export %s='%s'\n" "$tc_name" "$tc_quoted" ;;
    esac
  done
  if [ "${TC_PYTHON_SELECTED:-0}" -eq 1 ]; then
    case "$TC_OPERATION" in
      toolchain-github-env) printf 'PYTHONHOME=\n' >>"$GITHUB_ENV" ;;
      *) printf '%s\n' 'unset PYTHONHOME' ;;
    esac
  fi
  case "$TC_OPERATION" in
    toolchain-github-env)
      for tc_name in $TC_UNSET_VERSIONS; do printf '%s=\n' "$tc_name" >>"$GITHUB_ENV"; done
      ;;
    *)
      printf '%s\n' 'unset MISE_CONFIG_FILE GHCUP_USE_XDG_DIRS'
      # shellcheck disable=SC2016 # Code for the receiving shell, not this one.
      printf '%s\n' 'for _kio_override in $(env | sed -n '\''s/^\(MISE_[A-Za-z0-9_]*_VERSION\)=.*/\1/p'\''); do unset "$_kio_override"; done' 'unset _kio_override' ;;
  esac
}

toolchain_dispatch() {
  TC_GENERAL_DATA=${KIO_GENERAL_MISE_DATA_DIR-${MISE_DATA_DIR-}}
  TC_GENERAL_CACHE=${MISE_CACHE_DIR-}
  TC_GENERAL_STATE=${MISE_STATE_DIR-}
  TC_CALLER_CARGO_HOME=${CARGO_HOME-}
  TC_CALLER_CARGO_SET=${CARGO_HOME+x}
  TC_OPERATION=$1
  shift
  TC_ENVIRONMENT=development
  TC_CORE=0
  TC_PYTHON_SELECTED=0
  TC_REPORTS=0
  TC_REQUESTED_HELPERS=
  TC_LANGUAGES=
  while [ $# -gt 0 ]; do
    case "$1" in
      --environment) shift; [ $# -gt 0 ] || toolchain_fail '--environment requires an ID'; TC_ENVIRONMENT=$1 ;;
      --environment=*) TC_ENVIRONMENT=${1#*=} ;;
      --core) TC_CORE=1 ;;
      --reports) TC_REPORTS=1 ;;
      --helpers) shift; [ $# -gt 0 ] || toolchain_fail '--helpers requires tool IDs'; TC_REQUESTED_HELPERS=$(printf '%s' "$1" | tr ',' ' ') ;;
      --) shift; break ;;
      --*) toolchain_fail "unknown toolchain argument: $1" ;;
      *) TC_LANGUAGES="${TC_LANGUAGES:+$TC_LANGUAGES,}$1" ;;
    esac
    shift
  done
  if [ "$TC_OPERATION" != toolchain-bootstrap ]; then
    [ -n "$TC_LANGUAGES" ] || toolchain_fail 'select at least one target language'
  fi
  case "$TC_ENVIRONMENT" in ''|*[!a-zA-Z0-9_-]*) toolchain_fail 'invalid environment ID' ;; esac
  if [ "$TC_ENVIRONMENT" != development ]; then
    if [ ! -f "$REPO_ROOT/mise.$TC_ENVIRONMENT.toml" ] || [ ! -f "$REPO_ROOT/mise.$TC_ENVIRONMENT.lock" ]; then
      toolchain_fail "unknown or unlocked environment: $TC_ENVIRONMENT"
    fi
  fi
  TC_TOOLS=$(toolchain_tool_names "$TC_LANGUAGES")
  toolchain_platform
  case "$TC_OPERATION" in
    toolchain-plan|toolchain-install|toolchain-general-install) warn_toolchain_languages "$TC_LANGUAGES" ;;
  esac
  TC_ROOT=${KIO_TOOLCHAINS_ROOT:-$REPO_ROOT/.toolchains}
  case "$TC_ROOT" in /*|[A-Za-z]:/*|[A-Za-z]:\\*) ;; *) toolchain_fail 'KIO_TOOLCHAINS_ROOT must be absolute' ;; esac
  TC_ROOT=$(toolchain_native_path "$TC_ROOT")
  TC_ROOT=${TC_ROOT%/}
  [ -n "$TC_ROOT" ] || toolchain_fail 'the filesystem root cannot be a toolchain store'
  case "$TC_ROOT" in [A-Za-z]:) toolchain_fail 'a drive root cannot be a toolchain store' ;; esac
  if [ -d "$TC_ROOT" ]; then
    TC_ROOT=$(CDPATH='' cd -- "$TC_ROOT" && pwd -P)
    TC_ROOT=$(toolchain_native_path "$TC_ROOT")
  fi
  TC_DIGEST=$(toolchain_digest)
  case "$TC_OPERATION" in
    toolchain-key) printf '%s-%s-%s\n' "$TC_PLATFORM" "$TC_DIGEST" "$(printf '%s' "$TC_TOOLS" | toolchain_hash)"; return ;;
    toolchain-bootstrap|toolchain-install|toolchain-general-install) toolchain_lock ;;
  esac
  toolchain_mise
  if [ "$TC_OPERATION" = toolchain-bootstrap ]; then printf '%s\n' "$TC_MISE"; return; fi
  if [ "$TC_OPERATION" = toolchain-plan ]; then
    printf 'platform: %s\nenvironment: %s\nstore: %s\nrecipe: %s\n' "$TC_PLATFORM" "$TC_ENVIRONMENT" "$TC_ROOT" "$TC_DIGEST"
    for tc_tool in $TC_TOOLS; do printf '%s@%s\n' "$tc_tool" "$(toolchain_version "$tc_tool")"; done
    case " $TC_TOOLS " in *' ghcup '*)
      # shellcheck disable=SC1091
      . "$REPO_ROOT/tools/toolchains/ghcup.sh"
      toolchain_ghcup_version
      printf 'ghc@%s (GHCup delegated compiler)\n' "$tc_ghc" ;;
    esac
    if [ "${TC_PLATFORM%%-*}" = linux ]; then
      tc_impls=$(printf '%s' "$TC_LANGUAGES" | tr ', ' '\n' | sed '/^$/d; s/^kio@//; s/^/kio@/')
      tc_packages=$(emit_extra_system_packages "$tc_impls")
      if [ -n "$tc_packages" ]; then
        printf 'System prerequisites (mise bootstrap; root/sudo required for changes):\n%s\n' "$tc_packages"
      fi
    fi
    return
  fi
  toolchain_snapshot
  toolchain_controlled_environment
  if [ "$TC_OPERATION" = toolchain-install ]; then
    toolchain_install_selected
    if [ "$TC_CALLER_CARGO_SET" = x ]; then export CARGO_HOME="$TC_CALLER_CARGO_HOME"; else unset CARGO_HOME; fi
  fi
  toolchain_verify_selected
  TC_HELPERS=$TC_REQUESTED_HELPERS
  if [ "$TC_CORE" -eq 1 ]; then TC_HELPERS="${TC_HELPERS:+$TC_HELPERS }$(print_core_helper_tools)"; fi
  if [ "$TC_REPORTS" -eq 1 ]; then
    TC_HELPERS="${TC_HELPERS:+$TC_HELPERS }$REPORT_MISE_TOOLS"
  fi
  if [ -n "$TC_HELPERS" ]; then toolchain_verify_helpers; fi
  PATH="$(toolchain_shell_path "${TC_MISE%/*}"):$TC_PATH:$PATH"
  export PATH
  export KIO_TOOLCHAINS_ROOT="$TC_ROOT" KIO_TOOLCHAIN_ENVIRONMENT="$TC_ENVIRONMENT" KIO_TOOLCHAIN_IDENTITY="$TC_DIGEST"
  export KIO_GENERAL_MISE_DATA_DIR="${TC_GENERAL_DATA:-${XDG_DATA_HOME:-$HOME/.local/share}/mise}"
  if [ -n "${TC_RUST_VERSION:-}" ]; then export RUSTUP_TOOLCHAIN="$TC_RUST_VERSION"; fi
  if [ "$TC_OPERATION" = toolchain-general-install ]; then
    [ $# -gt 0 ] || toolchain_fail 'provide explicit general helper tools after --'
    if [ -n "$TC_GENERAL_DATA" ]; then export MISE_DATA_DIR="$TC_GENERAL_DATA"; else unset MISE_DATA_DIR; fi
    if [ -n "$TC_GENERAL_CACHE" ]; then export MISE_CACHE_DIR="$TC_GENERAL_CACHE"; else unset MISE_CACHE_DIR; fi
    if [ -n "$TC_GENERAL_STATE" ]; then export MISE_STATE_DIR="$TC_GENERAL_STATE"; else unset MISE_STATE_DIR; fi
    export MISE_SHARED_INSTALL_DIRS="$TC_ROOT/mise/data/installs"
    export MISE_GLOBAL_CONFIG_FILE="$TC_SNAPSHOT/optional/mise.toml"
    unset MISE_OFFLINE
    MISE_ENABLE_TOOLS="$MISE_ENABLE_TOOLS,$(toolchain_enable_names "$@")"
    if [ "$TC_CALLER_CARGO_SET" = x ]; then export CARGO_HOME="$TC_CALLER_CARGO_HOME"; else unset CARGO_HOME; fi
    tc_requested_helpers=$*
    set --
    for tc_tool in $tc_requested_helpers; do set -- "$@" "$tc_tool@$(toolchain_version "$tc_tool")"; done
    "$TC_MISE" install --locked -y "$@"
    return
  fi
  if [ "$TC_OPERATION" = toolchain-exec ]; then
    [ $# -gt 0 ] || toolchain_fail 'toolchain-exec requires -- command'
    exec "$@"
  fi
  case "$TC_OPERATION" in toolchain-env|toolchain-github-env) toolchain_export_environment ;; esac
}
