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
checker="$private/mise/data/installs/npm-typescript/$current/bin/tsc"
mv "$checker" "$checker.hidden"
if KIO_TOOLCHAINS_ROOT="$private" sh "$owner" toolchain-exec ts -- sh -c ': >"$1"' sh "$work/subject-ran" >"$work/missing.log" 2>&1; then
  printf 'error: incomplete tool installation executed its subject\n' >&2; exit 1
fi
[ ! -e "$work/subject-ran" ]
mv "$checker.hidden" "$checker"
mv "$checker" "$checker.saved"
ln -s "$store/mise/data/installs/npm-typescript/$current/lib/node_modules/typescript/bin/tsc" "$checker"
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
