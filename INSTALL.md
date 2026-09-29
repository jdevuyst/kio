# Installing Kio from a checkout

This guide sets up the pinned development environment used by Kio's
local checks and CI. For a minimal source build of only the `kio`
binary and VS Code extension, see
[`docs/guides/install-from-source.md`](docs/guides/install-from-source.md).

## Choose a setup path

The fastest full environment is the dev container at
[`.devcontainer/`](.devcontainer/):

- VS Code: open the checkout and choose "Reopen in Container".
- GitHub Codespaces: open a Codespace from the repository.
- Linux CI: uses the published dev container image, then installs
  selected backend extras with `ci/impl-toolchain.sh`.

For a local setup without the dev container, install
[mise](https://mise.jdx.dev), trust this checkout, and install the
pinned tools from `mise.toml` and `mise.lock`.

## What mise owns

`mise.toml`, `mise.optional.toml`, `mise.lock`, and
`mise.optional.lock` are the source of truth for language runtimes,
compilers, package managers, linters, formatters, and local CI helper
tools. Kio does not support repo tooling that cannot be installed
through mise. If a future tool genuinely requires a different route,
revise this policy in the same change before adding the tool.

Tools installed by a mise-managed package-manager backend count as
mise-owned: `cargo:*` entries use Cargo, `npm:*` entries use npm, and
pipx-backed entries use pipx, but the tool name and version still live
in the mise config and lock files. Do not add direct `cargo install`,
`npm install -g`, or `pipx install` setup steps.

The pinned set includes:

- language and backend tooling: Node.js, Rust, Python, Go, Java, Swift,
  and `ghcup` for GHC;
- repo and workflow tools: `tree-sitter`, `actionlint`, `zizmor`,
  `sccache`, `shellcheck`, `ripgrep`, pipx-backed `yamllint`,
  npm-backed TypeScript, `pyright`, and `markdownlint-cli2`, and
  `codeowners-validator`;
- optional report/audit CLIs: `cargo-fuzz`, `cargo-mutants`,
  `cargo-llvm-cov`, and `cargo-audit` through mise's `cargo:`
  backend;
- `wasm-pack` and `dist` (release binaries and installers) through
  mise's `aqua:` backend.

The Cargo-backed report/audit tools may still compile from source, so
the core dev container and baseline local setup do not install them.
They stay pinned in mise and are installed only when a deep report,
audit, or dependency-upgrade pass needs them.

Non-mise setup is limited to the mise bootstrap itself and OS/runtime
libraries required by mise-managed tools.

## Install mise

Linux:

```sh
curl https://mise.run | sh
echo 'eval "$(mise activate bash)"' >> ~/.bashrc
```

macOS:

```sh
brew install mise
echo 'eval "$(mise activate zsh)"' >> ~/.zshrc
```

If you use Bash on macOS, append `eval "$(mise activate bash)"` to
`~/.bashrc` instead.

Windows PowerShell:

```powershell
scoop install mise
Add-Content $PROFILE 'mise activate pwsh | Out-String | Invoke-Expression'
```

Restart the shell after adding activation, or run the activation command
in the current shell before continuing.

On Windows, use PowerShell for the mise installation and activation above,
then run the repository's `sh ci/...` entry points from Git Bash. The supported
Windows automation route uses the `sh` and `cygpath` supplied by Git for
Windows/MSYS to bridge shell paths to native tools.

## Install pinned tools

Linux and macOS:

```sh
mise trust mise.toml
sh ci/impl-toolchain.sh core-mise-tools | xargs mise install --locked -y
```

Windows:

```powershell
mise trust mise.toml
mise install --locked node npm:typescript python go java rust ghcup
```

The Linux and macOS command installs the core toolchain first. The
Windows set above is for local development; the Windows CI job installs
only `rust`, then runs the native scheduler and compiler tests, a compiler
build smoke, and a compile-only sweep of the success goldens. It does not
run emitted output or the POSIX-only integration harnesses. The Linux dev
container already carries the core toolchain.

Install optional report/audit harness tools only when you need them:

```sh
sh ci/impl-toolchain.sh install-report-tools
```

This installs `cargo-fuzz`, `cargo-mutants`, `cargo-llvm-cov`, and
`cargo-audit` via mise's `cargo:` backend. It is deliberately separate
from `core-mise-tools` because those CLIs may source-build.

If mise reports GitHub API rate-limit errors while resolving release
metadata, authenticate with the GitHub CLI (`gh auth login`) or set
`MISE_GITHUB_TOKEN` to a token that can read public release metadata.

## Install backend extras

`ci/impl-toolchain.sh` is the mapping from Kio implementation selectors
to the tools and system packages needed to run them.

All accepted impl selectors:

```sh
sh ci/impl-toolchain.sh impls
```

Impl selectors whose runner tools are currently on `PATH`:

```sh
sh ci/impl-toolchain.sh installed-impls
```

Always-on Linux shard impls:

```sh
sh ci/impl-toolchain.sh core-impls
```

Extra impls that CI samples from:

```sh
sh ci/impl-toolchain.sh extra-impls
```

On Linux, install tooling for the impls you intend to run:

```sh
sh ci/impl-toolchain.sh install kio@swift,kio@haskell
```

On macOS, install the mise-managed extra backend toolchains without the
Linux system-package step:

```sh
sh ci/impl-toolchain.sh extra-mise-tools kio@swift,kio@haskell | xargs mise install --locked -y
```

For a local Linux selected-shard run:

```sh
impls=$(sh ci/impl-toolchain.sh select --extra-count=3 --seed=local)
sh ci/impl-toolchain.sh install "$impls"
sh ci/all.sh "$impls"
```

On macOS, install the selected shard's mise tools before running it:

```sh
impls=$(sh ci/impl-toolchain.sh select --extra-count=3 --seed=local)
sh ci/impl-toolchain.sh extra-mise-tools "$impls" | xargs mise install --locked -y
sh ci/all.sh "$impls"
```

The helper invokes `mise install --locked` for backend tools. On Linux,
when a selected backend also needs distro shared libraries, it invokes
`mise bootstrap packages apply apt:...` with manager-qualified package
specs. `mise bootstrap` is experimental in mise, so the helper enables
it only for that command.

## Python for the Python backend runner

The Python backend's test runner imports emitted packages with the local
Python interpreter: `python3` on Linux/macOS, `python` on Windows.
`mise.toml` pins Python. The dev container also installs that pinned
Python through mise because the core YAML linter uses mise's pipx
backend.

## Put ghcup's GHC on PATH

`mise activate` exposes the pinned tools through mise's shims, but the
Haskell compiler is one indirection removed: `mise.toml` pins `ghcup`,
and `ghcup` installs `ghc` into `~/.ghcup/bin`, which is not a mise
shim. Building or running the Haskell backend needs that directory on
PATH explicitly:

```sh
echo 'export PATH="$HOME/.ghcup/bin:$PATH"' >> ~/.bashrc
```

The dev container reaches this differently: its Dockerfile sets
`GHCUP_INSTALL_BASE_PREFIX=/usr/local`, so `ghcup` installs `ghc` into
`/usr/local/.ghcup/bin` — not `$HOME/.ghcup/bin` — and that directory is
already on the image's `PATH`. So a container needs no extra step; a local
checkout, where `ghcup` uses the default `$HOME/.ghcup/bin`, must add the
line above. `go`, `java`, `swift`, `node`, and the rest are native mise
tools and need no extra PATH step beyond mise activation.

## Warm caches

The dev container pre-warms build caches. On a local checkout you can
reuse the container lifecycle scripts:

```sh
sh .devcontainer/on-create.sh
sh .devcontainer/update-content.sh
```

`on-create.sh` fetches Cargo and npm dependencies. `update-content.sh`
builds the Rust test binaries used by the checks. Both scripts are
POSIX sh, guarded, and idempotent.

## Build and check

Run a smoke build and the baseline local gate:

```sh
( cd kio-rs && sh ../ci/cargo.sh build )
sh ci/all.sh SAMPLE_IMPL
```

`SAMPLE_IMPL` runs one applicable implementation per case;
`FULL_IMPL_MATRIX` runs the full matrix, and a comma-separated impl list
restricts the run. See [`TESTING.md`](TESTING.md) § Local iteration for
the selector semantics.

Use explicit impls when you are validating backend behavior:

```sh
sh ci/impl-toolchain.sh install kio@python,kio@rust
sh ci/all.sh kio@python,kio@rust
```

For repeated Rust-heavy builds you can optionally wire in the
[sccache](https://github.com/mozilla/sccache) compiler cache — mise
provisions the binary, but nothing turns it on by default. Casual
contributors can skip it. To enable it, export the wrapper and pre-start
the daemon once per shell session:

```sh
export RUSTC_WRAPPER=sccache
export SCCACHE_IDLE_TIMEOUT=0
sccache --dist-status >/dev/null
```

The pre-start matters once the wrapper is set: if the first `rustc`
spawns the daemon lazily inside scheduler-admitted compiler work, the
daemon can pin that lease and deadlock later admission.
`SCCACHE_IDLE_TIMEOUT=0` keeps the daemon alive so a later idle shutdown
doesn't reopen that window. Repository entry points use an isolated generic
readiness hook for explicitly configured wrappers, but a wrapper hidden in
Cargo configuration still requires this session-level pre-start.

## Caches

The main Kio-owned caches that build up as you work are:

- **Kio semantic build caches** live in `out/.kio-cache/` under each
  package workdir and keep warm `kio check` / `build` / `test` / `doc`
  fast. `kio cache path` prints the location, `kio cache clear` empties
  it, `kio cache gc` sweeps stale entries, and `--no-cache` bypasses the
  cache for one run. The contract is [`specs/cli.md`](specs/cli.md) §
  `kio cache <subcommand>`.
- The **test-runner artifact cache** holds compiled per-golden binaries,
  reused across runs and sibling worktrees at
  `$XDG_CACHE_HOME/kio/<suite>/` (else `~/.cache/kio/<suite>/`). It is
  the largest Kio-owned thing on a dev machine's disk, and self-bounds
  through a per-impl-target LRU cap, so it rarely needs manual clearing.
- The **CI scheduler bootstrap cache** stores immutable helper binaries and
  keyed Cargo targets below `<git-common-dir>/kio-ci-scheduler/`. Sibling
  worktrees share it. It is fully rebuildable and normally stays warm; clearing
  it deliberately forces the next scheduler command to rebuild the helper.

## Troubleshooting

- `mise` says the config is untrusted: run `mise trust mise.toml` from
  the repository root.
- `python3` or `python` is missing: repair mise activation or run
  `mise install --locked python`; do not fall back to an OS Python for
  Kio checks.
- `ghc` is missing after `mise install`: add `~/.ghcup/bin` to PATH.
- Swift or Haskell backend runs fail on missing shared libraries: run
  `sh ci/impl-toolchain.sh install <impls>` for the impl list you are
  testing. That routes Linux system packages through mise bootstrap.
- A Cargo-backed mise tool takes a while to install: that is expected
  when mise's `cargo:` backend compiles the CLI from source.
