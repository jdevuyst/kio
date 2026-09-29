#!/bin/sh
#
# Print the path of a VS Code CLI usable from a lifecycle hook, or exit 1
# when there is none.
#
# Usage: find-code-cli.sh <server-root>...
#
# VS Code Server ships two CLIs under `<server-root>/bin/<commit>/bin/`
# (Codespaces interposes a platform level: `bin/<platform>/<commit>/…`):
#
#   remote-cli/code — talks to the running window over the socket named by
#     $VSCODE_IPC_HOOK_CLI. The server exports that variable into integrated
#     terminals, and nowhere else. Without it every command refuses with
#     "Command is only available in WSL or inside a Visual Studio Code
#     terminal" — and exits 0 while doing nothing, so a caller that trusts
#     exit status installs nothing and reports success.
#
#   code-server — the server's own CLI. Manages extensions directly in the
#     server-root-level `extensions/` directory (shared across server
#     versions), needs no window, no socket, and no $VSCODE_IPC_HOOK_CLI.
#     This is the one a lifecycle hook can use.
#
# The `code` on $PATH is a third thing: the Microsoft base image's
# forwarder, which execs the first *other* `code` on $PATH and exits 127
# with "code or code-insiders is not installed" when there is none. It
# reaches remote-cli only where the host puts remote-cli on $PATH — which
# for a lifecycle hook under local "Reopen in Container" never happens. It
# is the last resort here, not the first choice, and $PATH is fixed for the
# life of a process, so a caller cannot wait for it to start working.
#
# POSIX sh only.

set -eu

for root in "$@"; do
  for candidate in \
    "$root"/bin/*/bin/code-server \
    "$root"/bin/*/*/bin/code-server
  do
    # A server still unpacking leaves the path present but not yet
    # executable; the caller polls, so skipping is the right move.
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      exit 0
    fi
  done
done

if code --version >/dev/null 2>&1; then
  printf '%s\n' code
  exit 0
fi

exit 1
