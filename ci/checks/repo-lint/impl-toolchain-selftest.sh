#!/bin/sh
# Prove tool installation covers availability-driven checks without
# changing the implementation shard or installing absent, unselected extras.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
# Parse the real owner before the selection fixture substitutes its dispatch.
# shellcheck source=tools/toolchains/provision.sh disable=SC1091
. "${KIO_TEST_PROVISION_SOURCE:-$REPO_ROOT/tools/toolchains/provision.sh}"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fixture="$scratch/repo with spaces"
bin="$scratch/bin with spaces"
mkdir -p "$fixture/ci" "$fixture/tools/toolchains" "$bin"
# Redirect only OS metadata to a fixture; all package selection/install code
# runs unchanged, while the mise shim prevents host package mutation.
sed "s@/etc/os-release@$scratch/os-release@g" "$REPO_ROOT/ci/impl-toolchain.sh" >"$fixture/ci/impl-toolchain.sh"
for utility in sh dirname awk tr cksum sed; do
  ln -s "$(command -v "$utility")" "$bin/$utility"
done

assert_equal() {
  if [ "$1" != "$2" ]; then
    printf 'impl-toolchain-selftest: %s: expected <%s>, got <%s>\n' "$3" "$2" "$1" >&2
    exit 1
  fi
}

select_tools() {
  tooling=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" tooling-impls "$expected")
}

for row in '2 go go go' '3 java javac java' '1 swift swiftc swift' '7 haskell ghc ghcup'; do
  # shellcheck disable=SC2086
  set -- $row
  seed=$1
  target=$2
  compiler=$3
  mise_tool=$4
  expected=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" select --extra-count=3 --seed="$seed-1-linux")
  case ",$expected," in
    *",kio@$target,"*) printf 'fixture seed includes %s\n' "$target" >&2; exit 1 ;;
  esac

  select_tools
  assert_equal "$tooling" "$expected" "$target absent tooling"

  printf '#!/bin/sh\nexit 96\n' >"$bin/$compiler"
  select_tools
  assert_equal "$tooling" "$expected" "$target nonexecutable tooling"

  # An executable shim may fail until its pinned tool is installed. Probe
  # command availability, not successful execution of the ambient version.
  chmod +x "$bin/$compiler"
  select_tools
  assert_equal "$tooling" "$expected,kio@$target" "$target present tooling"
  tools=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" extra-mise-tools "$tooling")
  if ! printf '%s\n' "$tools" | grep -qx "$mise_tool"; then
    printf 'impl-toolchain-selftest: missing pinned %s installation\n' "$mise_tool" >&2
    exit 1
  fi
  if [ "$target" = go ]; then
    cat >"$fixture/tools/toolchains/provision.sh" <<'EOF'
# Dispatch fixture only: language and dependency selection use the real owner.
toolchain_dispatch() {
  printf '%s|%s|%s\n' "$1" "$2" "$(toolchain_tool_names "$2")" >>"$KIO_TEST_MISE_LOG"
}
EOF
    core=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" core-impls)
    core_tools=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" tooling-impls "$core")
    PATH="$bin" KIO_TEST_MISE_LOG="$scratch/mise.log" \
      sh "$fixture/ci/impl-toolchain.sh" install "$core_tools" >"$scratch/install.log"
    installed_tools=$(sed -n '1p' "$scratch/mise.log")
    assert_equal "$installed_tools" 'toolchain-install|js,ts,python,rust,go|node npm:typescript python npm:pyright rust go' 'ambient Go uses the same exact owned selection as the core tools'
  fi
  if [ "$target" = java ]; then
    installed=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" installed-impls)
    assert_equal "$installed" '' 'javac alone is not a Java runtime implementation'
  fi

  expected=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" impls)
  select_tools
  assert_equal "$tooling" "$expected" 'full tooling deduplicated'
  rm "$bin/$compiler"
done

cat >"$bin/uname" <<'EOF'
#!/bin/sh
printf '%s\n' "$KIO_TEST_UNAME"
EOF
cat >"$bin/mise" <<'EOF'
#!/bin/sh
printf '%s\n' "experimental=$MISE_EXPERIMENTAL $*" >>"$KIO_TEST_MISE_LOG"
EOF
chmod +x "$bin/uname" "$bin/mise"
for row in 'ubuntu 22.04 plain' 'ubuntu 24.04 t64' 'ubuntu 26.04 t64' 'debian 12 plain' 'debian 13 t64'; do
  # shellcheck disable=SC2086
  set -- $row
  printf 'ID=%s\nVERSION_ID=%s\n' "$1" "$2" >"$scratch/os-release"
  : >"$scratch/browser-mise.log"
  PATH="$bin" KIO_TEST_UNAME=Linux KIO_TEST_MISE_LOG="$scratch/browser-mise.log" \
    sh "$fixture/ci/impl-toolchain.sh" install-browser-system-packages --dry-run
  invocation=$(sed -n '1p' "$scratch/browser-mise.log")
  case "$3" in plain) suffix= ;; t64) suffix=t64 ;; esac
  for package in libasound2 libatk1.0-0 libglib2.0-0 libgtk-3-0; do
    case " $invocation " in
      *" apt:$package$suffix "*) ;;
      *) printf 'browser runtime: missing %s for %s %s\n' "$package$suffix" "$1" "$2" >&2; exit 1 ;;
    esac
  done
  case "$invocation" in
    'experimental=1 bootstrap packages apply --yes --update --dry-run '*) ;;
    *) printf 'browser runtime: not using the mise dry-run bootstrap: %s\n' "$invocation" >&2; exit 1 ;;
  esac
  for package in libdbus-glib-1-2 libxtst6 fonts-liberation; do
    case " $invocation " in
      *" apt:$package "*) ;;
      *) printf 'browser runtime: missing shared container/CI package %s\n' "$package" >&2; exit 1 ;;
    esac
  done
done
for platform in Darwin MINGW64_NT-10.0; do
  : >"$scratch/browser-mise.log"
  PATH="$bin" KIO_TEST_UNAME="$platform" KIO_TEST_MISE_LOG="$scratch/browser-mise.log" \
    sh "$fixture/ci/impl-toolchain.sh" install-browser-system-packages >"$scratch/browser-install.log"
  assert_equal "$(sed -n '1p' "$scratch/browser-mise.log")" '' "$platform has no browser OS package installer"
done
printf 'ID=unsupported\nVERSION_ID=1\n' >"$scratch/os-release"
if PATH="$bin" KIO_TEST_UNAME=Linux KIO_TEST_MISE_LOG="$scratch/browser-mise.log" \
  sh "$fixture/ci/impl-toolchain.sh" install-browser-system-packages >"$scratch/browser-error.log" 2>&1; then
  printf 'browser runtime: unsupported package route unexpectedly accepted\n' >&2
  exit 1
fi
assert_equal "$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" core-mise-tools | grep -cx http:kio-firefox)" 1 'same managed browser in the shared core tool list'

witness_bin=$scratch/witness-bin
witness_work=$scratch/witness-work
owned_pyright=$scratch/store/mise/data/installs/npm-pyright/fixture
mkdir -p "$witness_bin" "$witness_work" "$owned_pyright/node_modules/.bin" "$owned_pyright/bin"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$witness_bin/python3"
cp "$witness_bin/python3" "$witness_bin/python"
cat >"$witness_bin/mise" <<'EOF'
#!/bin/sh
printf '%s\n' "$KIO_TEST_SELECTED_PYRIGHT"
EOF
cat >"$owned_pyright/node_modules/.bin/pyright" <<'EOF'
#!/bin/sh
: >"$KIO_TEST_PYRIGHT_RAN"
EOF
cp "$owned_pyright/node_modules/.bin/pyright" "$owned_pyright/bin/pyright"
cp "$owned_pyright/node_modules/.bin/pyright" "$witness_bin/pyright"
chmod +x "$witness_bin/python3" "$witness_bin/python" "$witness_bin/mise" "$witness_bin/pyright" \
  "$owned_pyright/node_modules/.bin/pyright" "$owned_pyright/bin/pyright"
for layout in node_modules/.bin bin; do
  PATH="$owned_pyright/$layout:$witness_bin:$PATH" \
    KIO_TOOLCHAINS_ROOT="$scratch/store" \
    KIO_TEST_SELECTED_PYRIGHT="$owned_pyright/$layout/pyright" \
    KIO_TEST_PYRIGHT_RAN="$scratch/pyright-${layout##*/}-ran" \
    sh "$REPO_ROOT/ci/infra/toolchain-install-smoke.sh" --witness python "$witness_work"
  [ -e "$scratch/pyright-${layout##*/}-ran" ]
done
for selected in "$owned_pyright/node_modules/.bin/pyright" "$witness_bin/pyright"; do
  if PATH="$witness_bin:$PATH" KIO_TOOLCHAINS_ROOT="$scratch/store" \
    KIO_TEST_SELECTED_PYRIGHT="$selected" KIO_TEST_PYRIGHT_RAN="$scratch/ambient-pyright-ran" \
    sh "$REPO_ROOT/ci/infra/toolchain-install-smoke.sh" --witness python "$witness_work" \
    >"$scratch/pyright-error.log" 2>&1; then
    printf 'installer witness: ambient Pyright unexpectedly accepted\n' >&2
    exit 1
  fi
  grep -q 'error:.*Pyright' "$scratch/pyright-error.log"
done
[ ! -e "$scratch/ambient-pyright-ran" ]

printf 'impl-toolchain-selftest: pass (host selection; browser package mappings; installer Pyright ownership)\n'
