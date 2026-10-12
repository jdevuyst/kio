#!/bin/sh
# Prepare the offline manual before Cargo inventories the source crate.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
case "${1:-}" in
  package|publish) ;;
  *) printf '%s\n' 'usage: package-kio.sh package|publish [Cargo options]' >&2; exit 2 ;;
esac
manual_allow_dirty=false
for manual_arg in "$@"; do
  case "$manual_arg" in
    --manifest-path|--manifest-path=*|--workspace|--package|--package=*|--exclude|--exclude=*|-p|-p?*)
      printf '%s\n' 'package-kio.sh packages only the repository kio-lang crate' >&2
      exit 2
      ;;
    --allow-dirty) manual_allow_dirty=true ;;
    --offline|--frozen) CARGO_NET_OFFLINE=true; export CARGO_NET_OFFLINE ;;
  esac
done

# Cargo checks crate files; the canonical manual inputs live outside that crate.
if ! git -C "$REPO_ROOT" diff --quiet HEAD -- docs specs KIO-FOR-AGENTS.md; then
  printf '%s\n' 'manual sources have uncommitted changes; commit them before packaging' >&2
  exit 1
fi
if ! git -C "$REPO_ROOT" diff --quiet HEAD -- kio-rs; then
  printf '%s\n' 'package sources must be tracked and have no uncommitted changes' >&2
  exit 1
fi
cd "$REPO_ROOT/kio-rs"
sh "$SCRIPT_DIR/cargo.sh" run --locked --example pack_manual -- --write

# Cargo treats explicitly included ignored files as dirty too. Authenticate its
# exact source inventory before allowing only the freshly generated manual pair.
manual_scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$manual_scratch_parent"
manual_scratch=$(mktemp -d "$manual_scratch_parent/manual-package.XXXXXX")
trap 'rm -rf "$manual_scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
sh "$SCRIPT_DIR/cargo.sh" package --list --allow-dirty --locked >"$manual_scratch/files-native"
case "${OS:-}:${MSYSTEM:-}" in
  Windows_NT:*|*:MINGW*|*:MSYS*|*:CYGWIN*)
    tr '\134' '/' <"$manual_scratch/files-native" >"$manual_scratch/files"
    ;;
  *) cp "$manual_scratch/files-native" "$manual_scratch/files" ;;
esac
if LC_ALL=C grep -q '[[:cntrl:]]' "$manual_scratch/files" ||
  [ -n "$(sort "$manual_scratch/files" | uniq -d)" ]; then
  printf '%s\n' 'invalid or ambiguous package source paths' >&2
  exit 1
fi
check_package_sources() {
  set --
  while IFS= read -r manual_path; do
    case "$manual_path" in
      manual/index.tsv|manual/payload.tar.gz-concat|.cargo_vcs_info.json|Cargo.toml.orig) continue ;;
      ''|/*|../*|*/../*|*/..|./*|*/./*|*/.|*\\*)
        printf '%s\n' 'invalid package source path' >&2; return 1 ;;
    esac
    if [ ! -f "$REPO_ROOT/kio-rs/$manual_path" ]; then
      printf '%s\n' 'package sources must be regular files' >&2
      return 1
    fi
    manual_ancestor=kio-rs/$manual_path
    while :; do
      if [ -L "$REPO_ROOT/$manual_ancestor" ]; then
        printf '%s\n' 'package source files and ancestors must not be symlinks' >&2
        return 1
      fi
      case "$manual_ancestor" in */*) manual_ancestor=${manual_ancestor%/*} ;; *) break ;; esac
    done
    set -- "$@" "kio-rs/$manual_path"
  done <"$manual_scratch/files"
  if [ "$#" -eq 0 ] ||
    ! git -C "$REPO_ROOT" --literal-pathspecs ls-files --error-unmatch -- "$@" >/dev/null 2>&1 ||
    ! git -C "$REPO_ROOT" --literal-pathspecs diff --quiet HEAD -- "$@"; then
    printf '%s\n' 'package sources must be tracked and have no uncommitted changes' >&2
    return 1
  fi
}
check_package_sources
rm -rf "$manual_scratch"
trap - EXIT INT TERM HUP
if "$manual_allow_dirty"; then
  exec sh "$SCRIPT_DIR/cargo.sh" "$@"
fi
exec sh "$SCRIPT_DIR/cargo.sh" "$@" --allow-dirty
