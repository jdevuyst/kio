#!/bin/sh
# Prove tool installation covers availability-driven checks without
# changing the implementation shard or installing absent, unselected extras.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fixture="$scratch/repo with spaces"
bin="$scratch/bin with spaces"
mkdir -p "$fixture/ci" "$bin"
cp "$REPO_ROOT/ci/impl-toolchain.sh" "$fixture/ci/"
for utility in sh dirname awk tr cksum; do
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
    cat >"$bin/mise" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$KIO_TEST_MISE_LOG"
EOF
    chmod +x "$bin/mise"
    core=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" core-impls)
    core_tools=$(PATH="$bin" sh "$fixture/ci/impl-toolchain.sh" tooling-impls "$core")
    PATH="$bin" KIO_TEST_MISE_LOG="$scratch/mise.log" \
      sh "$fixture/ci/impl-toolchain.sh" install "$core_tools" >"$scratch/install.log"
    installed_tools=$(sed -n '/^install /p' "$scratch/mise.log")
    assert_equal "$installed_tools" 'install --locked -y go' 'ambient Go installed from locked pins'
    rm "$bin/mise"
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

printf 'impl-toolchain-selftest: pass (Go, Java, Swift, Haskell; selected/full; absent/nonexecutable/shim)\n'
