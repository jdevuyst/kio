#!/bin/sh
#
# Self-test for the dev container's VS Code CLI resolution
# (.devcontainer/find-code-cli.sh), which post-attach.sh uses to install the
# baked vscode-kio extension into the editor.
#
# Two properties are load-bearing, and a lifecycle hook that gets either
# wrong installs nothing while looking healthy:
#
#   1. The `code` on $PATH is the base image's forwarder, and the server's
#      remote-cli only reaches $PATH inside integrated terminals. In a hook
#      under local "Reopen in Container" the forwarder is permanently
#      non-functional (exit 127), so resolution must come from the server
#      tree itself. Scenario 1.
#
#   2. Within that tree, `bin/code-server` is the CLI a hook can use.
#      Its neighbour `bin/remote-cli/code` needs $VSCODE_IPC_HOOK_CLI, which
#      only integrated terminals get; without it, it refuses the command and
#      *exits 0*. Resolving it would turn the install into a silent no-op.
#      Scenario 2.
#
# Hermetic: fixture server trees under `mktemp`, and a $PATH scrubbed down to
# a scratch bin directory so a `code` on the host machine cannot decide the
# outcome.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

FIND_CLI="$REPO_ROOT/.devcontainer/find-code-cli.sh"
POST_ATTACH="$REPO_ROOT/.devcontainer/post-attach.sh"

for required in "$FIND_CLI" "$POST_ATTACH"; do
  if [ ! -f "$required" ]; then
    printf 'devcontainer-code-cli-selftest: cannot find %s\n' "$required" >&2
    exit 2
  fi
done

scratch=$(mktemp -d) || {
  printf 'devcontainer-code-cli-selftest: cannot make scratch dir\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

fail=0

# An empty $PATH entry: the resolver's own needs (printf, [) are shell
# builtins, so the only thing $PATH decides here is whether a `code` is
# reachable — which each scenario states for itself.
EMPTY_BIN="$scratch/empty-bin"
mkdir -p "$EMPTY_BIN"

# Plant an executable stub at $1, creating its parents.
plant() {
  mkdir -p "$(dirname -- "$1")"
  printf '#!/bin/sh\nexit 0\n' > "$1"
  chmod +x "$1"
}

# Plant a whole server tree under root $1, commit dir $2: both CLIs, as a
# real VS Code Server ships them.
plant_server() {
  plant "$1/bin/$2/bin/code-server"
  plant "$1/bin/$2/bin/remote-cli/code"
}

# Run the resolver against fixture roots with a scrubbed $PATH ($1 = the bin
# directory to expose, the rest = server roots). Sets `got` and `rc`. Called
# plainly, never through a command substitution — a subshell would strand
# `rc` in the child.
resolve() {
  bin_dir="$1"
  shift
  rc=0
  # Absolute interpreter: the scrubbed $PATH is for the resolver to search,
  # and would otherwise be searched for the shell that runs it.
  got=$(PATH="$bin_dir" /bin/sh "$FIND_CLI" "$@" 2>/dev/null) || rc=$?
}

# --- Scenario 1: local "Reopen in Container" — a server is installed, but no
# code is reachable through $PATH. The forwarder can never succeed here;
# resolution must come from the server tree. ---
local_root="$scratch/home/.vscode-server"
plant_server "$local_root" abc123
resolve "$EMPTY_BIN" "$local_root" "$scratch/absent"
if [ "$rc" -ne 0 ] || [ "$got" != "$local_root/bin/abc123/bin/code-server" ]; then
  printf 'devcontainer-code-cli-selftest: FAIL — an installed server was not resolved with no code on PATH (exit %s, got "%s")\n' \
    "$rc" "$got" >&2
  fail=1
fi

# --- Scenario 2: never remote-cli. It is the neighbour of the CLI we want,
# it exists in every server tree, and outside an integrated terminal it
# refuses the command and exits 0 — a silent no-op install. ---
case "$got" in
  *remote-cli*)
    printf 'devcontainer-code-cli-selftest: FAIL — resolved remote-cli, which no-ops outside a VS Code terminal (got "%s")\n' \
      "$got" >&2
    fail=1
    ;;
esac

# --- Scenario 3: Codespaces interposes a platform level under bin/. ---
cs_root="$scratch/vscode/vscode-server"
plant "$cs_root/bin/linux-x64/def456/bin/code-server"
plant "$cs_root/bin/linux-x64/def456/bin/remote-cli/code"
resolve "$EMPTY_BIN" "$scratch/absent" "$cs_root"
if [ "$rc" -ne 0 ] || [ "$got" != "$cs_root/bin/linux-x64/def456/bin/code-server" ]; then
  printf 'devcontainer-code-cli-selftest: FAIL — the Codespaces server layout was not resolved (exit %s, got "%s")\n' \
    "$rc" "$got" >&2
  fail=1
fi

# --- Scenario 4: the server tree wins over a working code on $PATH. That
# code is the forwarder, and what it forwards to cannot install from a
# hook. ---
path_bin="$scratch/path-bin"
plant "$path_bin/code"
resolve "$path_bin" "$local_root"
if [ "$rc" -ne 0 ] || [ "$got" != "$local_root/bin/abc123/bin/code-server" ]; then
  printf 'devcontainer-code-cli-selftest: FAIL — a code on PATH displaced the server tree (exit %s, got "%s")\n' \
    "$rc" "$got" >&2
  fail=1
fi

# --- Scenario 5: with no server tree to be found, a working code on $PATH is
# still worth a try. ---
resolve "$path_bin" "$scratch/absent"
if [ "$rc" -ne 0 ] || [ "$got" != code ]; then
  printf 'devcontainer-code-cli-selftest: FAIL — a working code on PATH was not used as the fallback (exit %s, got "%s")\n' \
    "$rc" "$got" >&2
  fail=1
fi

# --- Scenario 6: a server mid-unpack (path present, not yet executable) is
# not a resolution. This is the one case the caller's poll exists for, so it
# must report "not yet", not hand back an unusable path. ---
partial_root="$scratch/partial/.vscode-server"
mkdir -p "$partial_root/bin/abc123/bin"
: > "$partial_root/bin/abc123/bin/code-server"
resolve "$EMPTY_BIN" "$partial_root"
if [ "$rc" -eq 0 ]; then
  printf 'devcontainer-code-cli-selftest: FAIL — a non-executable code-server was resolved (got "%s")\n' "$got" >&2
  fail=1
fi

# --- Scenario 7: no editor anywhere — a headless `devcontainer up`. The
# resolver reports failure so the caller can no-op cleanly. ---
resolve "$EMPTY_BIN" "$scratch/absent"
if [ "$rc" -eq 0 ] || [ -n "$got" ]; then
  printf 'devcontainer-code-cli-selftest: FAIL — an absent editor resolved to something (exit %s, got "%s")\n' \
    "$rc" "$got" >&2
  fail=1
fi

# --- Scenario 8: post-attach.sh installs through the resolver. A hook that
# reaches for the bare code forwarder reintroduces the bug above, and the
# scenarios here would not see it. ---
if ! grep -q 'find-code-cli\.sh' "$POST_ATTACH"; then
  printf 'devcontainer-code-cli-selftest: FAIL — post-attach.sh does not resolve the CLI through find-code-cli.sh\n' >&2
  fail=1
fi
if sed 's/#.*//' "$POST_ATTACH" | grep -Eq '(^|[^-/[:alnum:]])code[[:space:]]+--'; then
  printf 'devcontainer-code-cli-selftest: FAIL — post-attach.sh invokes the bare code forwarder; use the resolved CLI\n' >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi

printf 'devcontainer-code-cli-selftest: ok (server-tree resolution, never remote-cli, Codespaces layout, PATH fallback, mid-unpack, no editor, hook wiring)\n'
exit 0
