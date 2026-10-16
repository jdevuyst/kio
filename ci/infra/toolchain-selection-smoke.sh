#!/bin/sh
# Independent selections, exported fragments, incomplete restores and setup reuse.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
owner=$repo_root/ci/impl-toolchain.sh
work=$(mktemp -d)
pid_a='' pid_b=''
cleanup() {
  [ -z "$pid_a" ] || kill "$pid_a" 2>/dev/null || true
  [ -z "$pid_b" ] || kill "$pid_b" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

snapshot_trust_smoke() {
  trust_main=$work/trust-main
  trust_checkout=$work/trust-worktree
  trust_store=$trust_checkout/.toolchains
  trust_owner=$trust_checkout/ci/impl-toolchain.sh
  original_store=$(sh "$owner" toolchain-plan js | sed -n 's/^store: //p')
  node_version=$(sh "$owner" toolchain-plan js | sed -n 's/^node@//p')
  clean_trust_env() {
    env -i HOME="$HOME" PATH="$PATH" TMPDIR="${TMPDIR:-$work}" "$@"
  }
  selected_trust_owner() {
    clean_trust_env KIO_TOOLCHAINS_ROOT="$trust_store" sh "$trust_owner" "$@"
  }
  clean_trust_env git init -q "$trust_main"
  clean_trust_env git -C "$trust_main" -c user.name=Fixture -c user.email=fixture@example.invalid \
    -c core.hooksPath=/dev/null commit -q --allow-empty -m fixture
  clean_trust_env git -C "$trust_main" worktree add -q --detach "$trust_checkout"
  mkdir -p "$trust_checkout/ci" "$trust_checkout/tools" "$trust_store/mise/data/installs/node"
  cp "$owner" "$trust_checkout/ci/"
  cp -R "$repo_root/tools/toolchains" "$trust_checkout/tools/"
  cp "$repo_root"/mise*.toml "$repo_root"/mise*.lock "$trust_checkout/"
  cp -R "$original_store/bootstrap" "$trust_store/"
  cp -R "$original_store/mise/data/installs/node/$node_version" "$trust_store/mise/data/installs/node/"
  ln -s "$trust_store" "$trust_main/.toolchains"
  [ -L "$trust_main/.toolchains" ]
  mkdir -p "$trust_store/mise/state/incomplete-installs/control/1"
  printf '%s\n' retained >"$trust_store/mise/state/incomplete-installs/control/1/incomplete"
  selected_trust_owner toolchain-install js >"$work/trust-first.log" 2>&1
  # shellcheck disable=SC2016 # Expand the selected child's configuration path.
  selected_trust_owner toolchain-exec js -- sh -ec 'mise trust --untrust "$MISE_SYSTEM_CONFIG_FILE"' >"$work/trust-untrust.log" 2>&1
  printf '\n' >>"$trust_checkout/mise.toml"
  if ! selected_trust_owner toolchain-install js >"$work/trust-second.log" 2>&1; then
    sed -n '1,100p' "$work/trust-second.log" >&2
    printf 'error: retained snapshot trust failed through a shared worktree store\n' >&2
    exit 1
  fi
  selected_trust_owner toolchain-install js >"$work/trust-reuse.log" 2>&1
  set -- "$trust_store"/config/*/mise.toml
  [ "$#" -eq 2 ]
  [ "$(cat "$trust_store/mise/state/incomplete-installs/control/1/incomplete")" = retained ]
  trust_native_store=$(selected_trust_owner toolchain-plan js | sed -n 's/^store: //p')
  selected_trust_owner toolchain-env js >"$work/trust-env.sh"
  # shellcheck disable=SC2016 # Check the exported child selection after sourcing it.
  clean_trust_env sh -ec '. "$1"; [ "$MISE_STATE_DIR" = "$2/mise/state" ]; [ "$MISE_TRUSTED_CONFIG_PATHS" = "$2/config" ]; node --version' \
    sh "$work/trust-env.sh" "$trust_native_store"
  selected_trust_owner toolchain-exec js --helpers tree-sitter -- tree-sitter --version
  mkdir "$work/caller-config" "$trust_store/config-lookalike" "$trust_store/config/trust-positive"
  printf '%s\n' '[env]' 'KIO_TRUST_PROBE = "caller"' >"$work/caller-config/mise.toml"
  cp "$work/caller-config/mise.toml" "$trust_store/config-lookalike/mise.toml"
  cp "$work/caller-config/mise.toml" "$trust_store/config/trust-positive/mise.toml"
  ln -s "$work/caller-config" "$trust_store/config/escape"
  # Paranoid mode isolates configured path trust from Git worktree inheritance.
  selected_trust_owner toolchain-exec js -- env MISE_OVERRIDE_CONFIG_FILENAMES=mise.toml \
    MISE_CEILING_PATHS="$work" MISE_PARANOID=1 mise -C "$trust_store/config/trust-positive" trust --show >"$work/trust-positive.log" 2>&1
  grep -q ': trusted$' "$work/trust-positive.log"
  for trust_negative in "$work/caller-config" "$trust_store/config-lookalike" "$trust_store/config/escape"; do
    selected_trust_owner toolchain-exec js -- env MISE_OVERRIDE_CONFIG_FILENAMES=mise.toml \
      MISE_CEILING_PATHS="$work" MISE_PARANOID=1 mise -C "$trust_negative" trust --show >"$work/trust-negative.log" 2>&1
    if ! grep -q ': untrusted$' "$work/trust-negative.log" || grep -q ': trusted$' "$work/trust-negative.log"; then
      sed -n '1,100p' "$work/trust-negative.log" >&2
      printf 'error: caller, sibling, or symlink escape inherited snapshot trust\n' >&2
      exit 1
    fi
  done
  printf 'snapshot trust smoke: pass (two generations, worktree symlink, reuse, shared state, helper, trust boundaries)\n'
}

snapshot_trust_smoke
[ "${1:-}" != --trust-only ] || exit 0
sh "$owner" toolchain-install ts
sh "$owner" toolchain-install ts --environment typescript5_0
sh "$owner" toolchain-env ts >"$work/current.sh"
sh "$owner" toolchain-env ts --environment typescript5_0 >"$work/minimum.sh"
current=$(sh "$owner" toolchain-plan ts | sed -n 's/^npm:typescript@//p')
minimum=$(sh "$owner" toolchain-plan ts --environment typescript5_0 | sed -n 's/^npm:typescript@//p')
store=$(sh "$owner" toolchain-plan ts | sed -n 's/^store: //p')
[ "$current" != "$minimum" ] || { printf 'error: selection smoke requires distinct checker versions\n' >&2; exit 2; }
mkdir "$work/outside"
printf '%s\n' '[tools]' 'node = "0.0.0"' '"npm:typescript" = "0.0.0"' >"$work/outside/mise.toml"
printf '%s\n' 'node 0.0.0' >"$work/.tool-versions"
printf '%s\n' '[tools]' 'node = "0.0.0"' >"$work/.miserc.toml"
for name in ready-a ready-b go-a go-b; do mkfifo "$work/$name"; done
cat >"$work/check" <<'EOF'
#!/bin/sh
set -eu
ready_sent=0
trap 'if [ "$ready_sent" = 0 ]; then printf "failed\n" >"$2/ready-$4"; fi' EXIT
. "$1"
[ -z "${MISE_NODE_VERSION:-}" ]
[ -z "${MISE_NPM_TYPESCRIPT_VERSION:-}" ]
cd "$2/outside"
[ "$(tsc --version)" = "Version $3" ]
[ "$("$(mise which tsc)" --version)" = "Version $3" ]
case "$(mise which tsc)" in "$KIO_TOOLCHAINS_ROOT/mise/data/installs/npm-typescript/$3/"*) ;; *) exit 1 ;; esac
printf 'ready\n' >"$2/ready-$4"
ready_sent=1
IFS= read -r release <"$2/go-$4"
[ "$release" = go ]
[ "$(tsc --version)" = "Version $3" ]
[ "$(pwd)" = "$2/outside" ]
node -e 'if (20 + 22 !== 42) process.exit(1)'
EOF
MISE_NODE_VERSION=0.0.0 MISE_NPM_TYPESCRIPT_VERSION=0.0.0 sh "$work/check" "$work/current.sh" "$work" "$current" a &
pid_a=$!
MISE_NODE_VERSION=0.0.0 MISE_NPM_TYPESCRIPT_VERSION=0.0.0 sh "$work/check" "$work/minimum.sh" "$work" "$minimum" b &
pid_b=$!
IFS= read -r ready <"$work/ready-a"
[ "$ready" = ready ]
IFS= read -r ready <"$work/ready-b"
[ "$ready" = ready ]
printf 'go\n' >"$work/go-a"
printf 'go\n' >"$work/go-b"
wait "$pid_a"; pid_a=
wait "$pid_b"; pid_b=

# Damage only a private copy, while the original complete tools remain on PATH.
private="$work/private store"
mkdir -p "$private/mise/data/installs"
cp -R "$store/bootstrap" "$private/"
cp -R "$store/mise/data/installs/node" "$private/mise/data/installs/"
# Exercise a real cold npm install without an account mise on PATH.
PATH=/usr/bin:/bin KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-install ts
checker=$(KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-exec ts -- sh -c 'command -v tsc')
original_checker=$(sh "$owner" toolchain-exec ts -- sh -c 'command -v tsc')
mv "$checker" "$checker.hidden"
if KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-exec ts -- sh -c ': >"$1"' sh "$work/subject-ran" >"$work/missing.log" 2>&1; then
  printf 'error: incomplete tool installation executed its subject\n' >&2; exit 1
fi
[ ! -e "$work/subject-ran" ]
mv "$checker.hidden" "$checker"
mv "$checker" "$checker.saved"
ln -s "$original_checker" "$checker"
if KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-exec ts -- sh -c ': >"$1"' sh "$work/subject-ran" >"$work/origin.log" 2>&1; then
  printf 'error: escaped tool symlink executed its subject\n' >&2; exit 1
fi
[ ! -e "$work/subject-ran" ]
rm "$checker"
mv "$checker.saved" "$checker"
KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-install ts >"$work/setup-a.log" 2>&1 &
pid_a=$!
KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-install ts >"$work/setup-b.log" 2>&1 &
pid_b=$!
wait "$pid_a"; pid_a=
wait "$pid_b"; pid_b=
KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-exec ts -- tsc --version
printf 'toolchain selection smoke: pass (concurrent versions, cwd, nested lookup, exports, missing executable, shared setup)\n'
