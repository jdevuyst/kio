---
name: clear-caches
description: Free disk by wiping worktree-local caches — Cargo target/ dirs, kio build cache, golden/POC out/ trees, node_modules, tree-sitter build. Never touches ~/.cargo, ~/.npm, sibling worktrees, or the shared machine-wide caches (separate explicit steps)
allowed-tools: Bash
---

# Clear caches (kio)

Wipe everything cheap-to-rebuild that lives in this working tree. Useful when disk pressure interrupts a build — `kio-rs/target/` and the per-golden `out/` trees are the usual culprits.

The repo-wide cache index — every cache, its blast radius, and its owning doc — is [`ai/topics/caches.md`](../../topics/caches.md); this skill is the clearing procedure for the worktree-local family plus the deliberate machine-wide steps below.

Emission cases deliberately create their build tree and `--cache-base` under the orchestrator temporary root, then remove them with that run. They leave no `test-data/emissions/**/workdir/out/` tree and never use the shared runner artifact cache, so this skill has no emission path to clear.

## Worktree-local vs. shared

Four storage scopes have different blast radii:

- **Worktree-local** — `kio-rs/target/` and the other Cargo `target/` dirs, the kio **build** cache (`out/.kio-cache/` inside each workdir, cleared per-package by `kio cache clear`), the golden / POC `out/` trees, and `node_modules` / tree-sitter build. These belong to this working tree alone; wiping them disrupts nothing else. This is what the default run below clears.
- **Shared, machine-wide** — the test-runner **artifact** cache (`KIO_TEST_RUNNER_BUILD_CACHE_DIR`), which the orchestrators default to `$XDG_CACHE_HOME/kio/<suite>/` (else `~/.cache/kio/<suite>/`) so sibling worktrees reuse one cache. Clearing it affects **every** parallel worktree, exactly like clearing `~/.cache/sccache`. It is content-addressed (a stale entry is never *wrong*, only disk) and self-bounding (size-LRU), so manual clearing is rarely warranted — disk pressure only. The default run **does not touch it**; see § Shared test-runner artifact cache below for the deliberate, separate step.
- **Shared within this repository** — immutable CI-scheduler binaries and their keyed Cargo targets live below the Git common directory, so sibling worktrees reuse the exact helper build. The default run **does not touch them**; see § Shared scheduler bootstrap cache below.
- **Docker daemon state** — local devcontainer images and BuildKit layer cache live under the Docker daemon's storage root, outside the checkout, and are shared by every Docker build using that daemon. The default run **does not touch Docker**; see § Docker / BuildKit cache below for the deliberate, separate step.

## Scope

The default run is worktree-local only. **Never** touches:

- `~/.cargo/registry/`, `~/.cargo/git/`, `~/.npm/`, `~/.cache/sccache/` — out of repo. Slower to re-download / re-populate and not the source of worktree-local disk pressure.
- The shared test-runner artifact cache under `~/.cache/kio/` (or `$XDG_CACHE_HOME/kio/`) — machine-wide, shared across worktrees; clearing it is a separate explicit step (below), not part of the default wipe.
- The CI scheduler bootstrap cache below the Git common directory — shared by sibling worktrees; clearing it is a separate explicit step (below), not part of the default wipe.
- Docker images and BuildKit cache — daemon-wide, shared across builds; clearing them is a separate explicit step (below), not part of the default wipe.
- Sibling worktrees of this checkout (`git worktree list` enumerates them) — they have their own `target/` and `out/` trees; wiping theirs could disrupt parallel work.

## What it clears

| Path | What rebuilds it |
| --- | --- |
| `kio-rs/target/` | `cargo build` against `~/.cargo/registry/` (local). |
| `kio-rs/fuzz/target/` (if present) | `cargo +nightly fuzz build`. |
| `ci/infra/kio-ci-scheduler-rs/target/` | `cargo build` in the crate. |
| `ci/infra/kio-gen-rs/target/` | `cargo build` in the crate. |
| `ci/infra/kio-prime-check-rs/target/` | `cargo build` in the crate. |
| `ci/infra/kio-test-runner-rs/target/` | `cargo build --no-default-features --features {js,rust}` per the hygiene script. |
| `.kio-cache/` (`goldens/`, `kio-gen/`, `kiodoc/`, `poc/`) | Worktree-local artifact-cache tree, present when a job pinned `--cache-base` worktree-local. Content-addressed; re-warms case-by-case. Distinct from the shared cache under `~/.cache/kio/`. |
| `test-data/goldens/**/out/` (incl. the kio build cache `out/.kio-cache/`) | Re-created on next `kio build` per golden. |
| `test-data/poc/**/out/` (incl. the kio build cache `out/.kio-cache/`) | Re-created on next `kio build` per POC. |
| `ci/infra/highlight-agreement-js/node_modules/` | `npm install` against `~/.npm` (local). |
| `tools/vscode-kio/node_modules/` | `npm install`. |
| `tools/tree-sitter-kio/node_modules/` | `npm install`. |
| `tools/tree-sitter-kio/build/` | `npm install` plus the tree-sitter regen step. |

## Preflight

Refuse to run if `ci/all.sh`, `cargo`, `kio-ci-scheduler`, or `kio-test-runner-*` is active on the machine. Wiping `target/` mid-build corrupts the build; wiping `.kio-cache/` mid-run loses warm entries another process is reading. Take one point-in-time all-process snapshot with PID, stable accounting name (`ucomm`), and full argv in that order. Linux truncates `ucomm` to 15 bytes, so the scheduler's `kio-ci-schedule` prefix counts only when argv also names the complete `kio-ci-scheduler` executable; macOS may report the complete accounting name. The other watched accounting names contain no whitespace, so the leading PID, next whitespace-collapsed name token, and remaining argv can be inspected from the same row; executable paths and decorated `argv[0]` do not affect Cargo or runner detection. A `ci/all.sh`, `./all.sh`, or bare `all.sh` mention counts only for a shell-shaped accounting name; remote shells and non-shell editor, search, and cache commands do not count. BusyBox and Toybox count only when their applet is shell-shaped. A full argv display cannot portably distinguish a script operand from command text or an option value, so a matching shell mention conservatively refuses; bare `all.sh` is likewise conservative because `ps` does not expose the working directory portably. Bracketed first characters keep the patterns from matching their own text. If a snapshot row cannot supply those three fields, or process inspection otherwise fails, refuse because the safety check could not complete. This snapshot cannot prevent a relevant process from starting afterward, so run the wipe immediately after a successful preflight.

```sh
ci_script='(([^[:space:]]*/)?[c]i/[a]ll[.]sh|([.]/)?[a]ll[.]sh)([[:space:]]|$)'
ci_mention="(^|[[:space:]])$ci_script"
box_shell_pattern="(^|[[:space:]])([^[:space:]]*/)?(busy|toy)[b]ox[[:space:]]+[^[:space:]]*[s]h([[:space:]]+[^[:space:]]+)*[[:space:]]+$ci_script"
scheduler_pattern='(^|[[:space:]])([^[:space:]]*/)?[k]io-ci-scheduler([.]exe)?([[:space:]]|$)'

for process_tool in ps grep; do
  if ! command -v "$process_tool" >/dev/null 2>&1; then
    printf 'clear-caches: refusing — %s is unavailable, so active builds cannot be checked.\n' "$process_tool" >&2
    exit 1
  fi
done

ps_status=0
process_table=$(ps -Aww -o pid= -o ucomm= -o args= 2>/dev/null) || ps_status=$?
if [ "$ps_status" -ne 0 ] || [ -z "$process_table" ]; then
  printf 'clear-caches: refusing — process inspection failed (ps snapshot status %s).\n' "$ps_status" >&2
  exit 1
fi

inspection_status=0
process_ifs=$(printf ' \011')
printf '%s\n' "$process_table" |
  while IFS="$process_ifs" read -r process_pid process_name process_args; do
    case $process_pid in
      ''|*[![:digit:]]*) exit 11 ;;
    esac
    [ -n "$process_name" ] && [ -n "$process_args" ] || exit 11

    case $process_name in
      [c]argo|[k]io-ci-scheduler|[k]io-ci-scheduler[.]exe|[k]io-test-runner|[k]io-test-runner-*) exit 10 ;;
      [k]io-ci-schedule) process_pattern=$scheduler_pattern ;;
      [s]sh|[s]sh[[:digit:]]*|[s]sh[.-][[:digit:]]*|\
      [r]sh|[r]sh[[:digit:]]*|[r]sh[.-][[:digit:]]*|\
      [a]utossh|[a]utossh[[:digit:]]*|[a]utossh[.-][[:digit:]]*) continue ;;
      [b]usybox|[t]oybox) process_pattern=$box_shell_pattern ;;
      *[s]h|*[s]h[[:digit:]]*|*[s]h[.-][[:digit:]]*) process_pattern=$ci_mention ;;
      *) continue ;;
    esac

    process_grep_status=0
    printf '%s\n' "$process_args" | grep -Eq "$process_pattern" ||
      process_grep_status=$?
    case $process_grep_status in
      0) exit 10 ;;
      1) ;;
      *) exit 11 ;;
    esac
  done ||
  inspection_status=$?

case $inspection_status in
  0) ;;
  10)
    printf '%s\n' 'clear-caches: refusing — a build or test process is active. Wait, or stop it first.' >&2
    exit 1
    ;;
  *)
    printf '%s\n' 'clear-caches: refusing — process inspection failed while correlating process details.' >&2
    exit 1
    ;;
esac
```

## Run

Print disk free before. Sum the sizes of each target. Wipe. Print disk free after. Report freed bytes.

```sh
REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

before_avail=$(df --output=avail -B1 . | tail -1)

# Sum sizes of what's about to go (silently skip absent paths).
du -shc \
  kio-rs/target kio-rs/fuzz/target \
  ci/infra/kio-ci-scheduler-rs/target \
  ci/infra/kio-gen-rs/target \
  ci/infra/kio-prime-check-rs/target \
  ci/infra/kio-test-runner-rs/target \
  .kio-cache \
  ci/infra/highlight-agreement-js/node_modules \
  tools/vscode-kio/node_modules \
  tools/tree-sitter-kio/node_modules tools/tree-sitter-kio/build \
  2>/dev/null | tail -1

# Per-golden + per-POC out/ dirs (variable count; use find so the argv stays bounded).
find test-data/goldens -type d -name out -prune -print0 2>/dev/null \
  | xargs -0 -r du -shc 2>/dev/null | tail -1
find test-data/poc -type d -name out -prune -print0 2>/dev/null \
  | xargs -0 -r du -shc 2>/dev/null | tail -1

# Wipe.
rm -rf \
  kio-rs/target kio-rs/fuzz/target \
  ci/infra/kio-ci-scheduler-rs/target \
  ci/infra/kio-gen-rs/target \
  ci/infra/kio-prime-check-rs/target \
  ci/infra/kio-test-runner-rs/target \
  .kio-cache \
  ci/infra/highlight-agreement-js/node_modules \
  tools/vscode-kio/node_modules \
  tools/tree-sitter-kio/node_modules tools/tree-sitter-kio/build

find test-data/goldens -type d -name out -prune -exec rm -rf {} +
find test-data/poc -type d -name out -prune -exec rm -rf {} +

after_avail=$(df --output=avail -B1 . | tail -1)
freed=$(( after_avail - before_avail ))
printf 'clear-caches: freed %s on %s\n' \
  "$(numfmt --to=iec --suffix=B "$freed")" \
  "$(df --output=source . | tail -1)"
```

## Shared test-runner artifact cache

The orchestrators back the test-runner artifact cache (`KIO_TEST_RUNNER_BUILD_CACHE_DIR`) with a machine-stable shared root so sibling worktrees reuse compiled binaries:

- Location: `$XDG_CACHE_HOME/kio/` when `XDG_CACHE_HOME` is set, else `~/.cache/kio/`, with a `<suite>/<target>/` subtree per corpus + impl-target.
- Knobs: `KIO_TEST_RUNNER_BUILD_CACHE_SIZE` (the size-LRU byte budget the orchestrators default; lower it to evict harder under disk pressure) and `KIO_TEST_RUNNER_BUILD_CACHE_DIR` (the root itself). See [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Compiler cache.

Clearing it is **machine-wide** — it drops warm entries every parallel worktree shares, like `sccache --stop-server && rm -rf ~/.cache/sccache`. Because it is content-addressed (a stale entry is never *wrong*, only disk) and self-bounding (the size-LRU caps it), manual clearing is rarely needed: reach for it only under genuine disk pressure, never as routine hygiene. Confirm with the user before wiping a shared cache other worktrees may be reading. The same mid-run refusal applies — do not wipe it while any `ci/all.sh` / runner is active. When you do clear it, target the kio subtree only, not the sibling `sccache` cache:

```sh
# Machine-wide. Run only under disk pressure, with no active run, after
# confirming no sibling worktree is mid-suite.
shared_root="${XDG_CACHE_HOME:-$HOME/.cache}/kio"
du -shc "$shared_root" 2>/dev/null | tail -1
rm -rf "$shared_root"
```

## Shared scheduler bootstrap cache

`ci/schedule.sh --prepare` stores immutable keyed binaries and their Cargo
targets below `<git-common-dir>/kio-ci-scheduler/`. The cache is rebuildable but
shared by every worktree registered to the repository. Clear it only under disk
pressure, after the same process preflight proves that no Cargo, scheduler,
runner, or broad-gate process is active:

```sh
repo_root=$(git rev-parse --show-toplevel)
git_common=$(git -C "$repo_root" rev-parse \
  --path-format=absolute --git-common-dir) || exit $?
case "$git_common" in
  /*) ;;
  [A-Za-z]:[\\/]*)
    command -v cygpath >/dev/null 2>&1 || {
      printf '%s\n' 'clear-caches: cygpath is required on Windows.' >&2
      exit 2
    }
    git_common=$(cygpath -u "$git_common") || exit $?
    ;;
  *)
    printf 'clear-caches: non-absolute Git common directory: %s\n' \
      "$git_common" >&2
    exit 2
    ;;
esac
scheduler_cache=$git_common/kio-ci-scheduler
du -sh "$scheduler_cache" 2>/dev/null || true
rm -rf "$scheduler_cache"
```

The next scheduler entry point rebuilds one exact key before fan-out. This step
does not touch worktree-local Cargo targets or the machine-wide sccache.

## Docker / BuildKit cache

Local devcontainer test builds can leave both a tagged image and BuildKit layer cache. This state is daemon-wide, not worktree-local. Do not clear it while any Docker build is active, and do not include it in the default cache wipe.

Inspect first:

```sh
docker system df
docker image ls 'kio-devcontainer-local'
```

Remove known local test tags explicitly before reaching for broad pruning:

```sh
docker image rm kio-devcontainer-local:test
```

If Docker's BuildKit cache is still the disk-pressure source, prune it as a separate, deliberate operation after confirming no other build on the machine needs those warm layers:

```sh
docker builder prune
```

See [`ai/topics/local-tools.md`](../../topics/local-tools.md) § Dev container Docker builds for the local devcontainer build path.

## Report

After running, summarize for the user:

- Filesystem the wipe ran on (so they know whether `/tmp` / another mount stayed full).
- Bytes freed.
- Per-target sizes before the wipe (one line each), so the user can spot which cache was the biggest contributor.
