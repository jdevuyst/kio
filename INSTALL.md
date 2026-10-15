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
- GitHub Pages: builds the website in the published dev container image.

Linux CI runs directly on hosted Ubuntu, using the same provisioning entry
points as local development.

For a local setup without the dev container, run
`sh ci/impl-toolchain.sh install-core`. It bootstraps and verifies the pinned
[mise](https://mise.jdx.dev) automatically.

## Tool ownership and storage

`mise.toml`, `mise.optional.toml`, `mise.lock`, and
`mise.optional.lock` own exact tool requests and resolved downloads.
Additional exact environments use `mise.<environment>.toml` and their
matching lock files. `development` selects the root configuration.

Swift uses native mise archive selection, not a checked-in per-archive digest.
Linux setup supplies system GnuPG and requires upstream signature verification
with a project-owned keyring; macOS uses the native package route.

Target-language toolchains live under the gitignored project directory
`.toolchains/`. This includes languages also used to build or check Kio:
Node.js, TypeScript, Python, and Rust are reused by core development and
backend checks. General helper CLIs stay in ordinary mise storage.

Mise installs direct tools where suitable. Delegated managers have explicit
owners under `tools/toolchains/`: `mise.version` and `mise.sha256` pin the
bootstrap, `rustup.toml` pins Rustup and its bootstrap checksums, and
`ghcup.toml` pins child GHC versions and verified upstream metadata. GHCup
itself is pinned in mise. A manager version alone does not pin its compiler.
The scripts in that directory implement these routes; `ci/impl-toolchain.sh`
owns language selection, setup, verification, and execution.

Tools installed by a mise-managed package-manager backend count as
mise-owned: `cargo:*` entries use Cargo, `npm:*` entries use npm, and
pipx-backed entries use pipx, but the tool name and version still live
in the mise config and lock files. Do not add direct `cargo install`,
`npm install -g`, or `pipx install` setup steps.

The pinned inventory includes:

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
  mise's `aqua:` backend;
- Firefox for website smoke checks and rendered captures through mise's
  `http:` backend, with the same pinned Playwright browser revision on every OS.

The Cargo-backed report/audit tools may still compile from source, so
the core dev container and baseline local setup do not install them.
They stay pinned in mise and are installed only when a deep report,
audit, or dependency-upgrade pass needs them.

Owned Rustup and GHCup child stores also sit below `.toolchains/`. Setup
does not change account-wide compiler defaults or shell profiles. Cargo
configuration, credentials, and configured compiler caches remain available
to repository builds. OS libraries and native SDKs remain external prerequisites;
the project directory does not contain a complete operating system.

Set `KIO_TOOLCHAINS_ROOT` to an absolute directory before setup and execution
to share an explicitly chosen store across worktrees. Keep that value consistent
for its users. Installations can coexist, and each command selects its own exact
environment. Remove or replace a store only while none of its tools are running.
Deleting `.toolchains/` removes project tool installations and download caches;
the tracked pins remain available for reinstalling. General mise helpers are
unaffected.

## Native prerequisites

Run the repository's `sh ci/...` entry points from a POSIX shell on Linux/macOS
or Git Bash on Windows. Windows setup requires the `sh` and `cygpath` supplied
by Git for Windows/MSYS to bridge shell paths to native tools. Downloads require
`curl`, and verification requires `sha256sum` or `shasum`.

Rust builds also need the platform's native compiler/linker environment. On
Debian/Ubuntu, provide the C/C++ compiler, linker, and development headers
supplied by `build-essential`. On macOS, install Xcode's command-line tools
and SDK. Windows uses Rust's MSVC toolchain and needs Visual Studio Build Tools
with the C++ build tools and Windows SDK; Git Bash alone does not supply them.
These native prerequisites are separate from managed compiler downloads.

The installer obtains the exact checksum-verified mise bootstrap below
`.toolchains/`; no account mise installation, shell activation, or profile edit
is required. Existing mise sessions can continue to manage general tools.

## Install the core development environment

From the checkout root (Git Bash on Windows):

```sh
sh ci/impl-toolchain.sh install-core
```

The core command installs Node.js, TypeScript, Python, and Rust into the
project store, plus the general development helpers through ordinary mise.
It does not install every target language or optional report tool. On Windows,
the same command selects the supported Rust/compiler development environment
without the unsupported general helper pool. The Linux dev container already
carries its core environment.

On macOS, core setup includes Python for helper dependencies and warns that this
additional setup is best-effort. For the supported native portability tools alone,
use `sh ci/install-target-language-toolchains.sh --portability-set`.

Development setup supports all eight target languages on the supported Linux
environment. The macOS supported set is JavaScript, TypeScript, Rust, and Swift;
the Windows supported set is Rust. These match the native GitHub portability
jobs. Explicit requests for other macOS/Windows languages print a best-effort
warning before setup and may need separately installed native prerequisites.
The full POSIX integration suite is not implied by the smaller native jobs.

Website browser tasks use Firefox on Linux x64/arm64 with an Ubuntu 22.04-compatible
GTK3 runtime, macOS 14+ x64/arm64, and Windows 11+ x64. The runtime-package helper
installs Linux libraries for Ubuntu 22.04/24.04/26.04 and Debian 12/13 through
mise bootstrap; it does nothing on macOS or Windows. The browser is installed,
versioned and removed by mise, not by an OS browser installer. It uses Playwright's
Firefox automation defaults with the normal OS content sandbox. Those defaults
turn off cross-process iframes; these localhost checks do not certify retail
Firefox's isolation policy. A sandbox or library failure is a setup error, not
an instruction to disable the sandbox or skip the browser check.

Install optional report/audit harness tools only when you need them:

```sh
sh ci/impl-toolchain.sh install-report-tools
```

This installs `cargo-fuzz`, `cargo-mutants`, `cargo-llvm-cov`, and
`cargo-audit` via mise's `cargo:` backend. It is deliberately separate
from core setup because those CLIs may source-build. It also prepares the
pinned fuzz Rust environment. Use
`toolchain-exec rust --environment optional --reports -- COMMAND`
for stable report/audit commands; fuzzing selects its separate pinned environment.

If mise reports GitHub API rate-limit errors while resolving release
metadata, authenticate with the GitHub CLI (`gh auth login`) or set
`MISE_GITHUB_TOKEN` to a token that can read public release metadata.

## Select target-language toolchains

Inspect the choices without installing anything:

```sh
sh ci/install-target-language-toolchains.sh
sh ci/install-target-language-toolchains.sh --list
```

Plan inspection reads the native mise configuration. If the pinned mise is not
already available, bootstrap that manager alone before inspecting a plan:

```sh
sh ci/impl-toolchain.sh toolchain-bootstrap
sh ci/install-target-language-toolchains.sh swift haskell --plan
```

`--plan` itself does not download or install target-language tools.

With no arguments the installer displays help. Request individual languages,
the supported set for this OS, or its smaller portability set:

```sh
sh ci/install-target-language-toolchains.sh swift haskell
sh ci/install-target-language-toolchains.sh --supported-set
sh ci/install-target-language-toolchains.sh --portability-set
```

The Linux portability set is JavaScript, TypeScript, and Rust; macOS adds
Swift; Windows selects Rust. The portability set is also available on Linux.
Set selection installs the ordinary development environment, not every retained
exact version. Selecting a smaller set does not satisfy a check that requires
all eight languages.

Setup reuses verified installations and checks the selected executable origins
and versions. Missing or mismatched tools fail; ambient account tools are not
a substitute. On Linux, the shared owner installs needed distro libraries with
`mise bootstrap packages apply` and manager-qualified package specs such as
`apt:libgmp-dev`. This can require administrative privileges. On macOS, Swift
requires Xcode's command-line tools and SDK. Git Bash is required on Windows;
it does not supply every ecosystem's native build prerequisite.

`ci/impl-toolchain.sh` also maps corpus implementation selectors to their
required tools. Inspect that separate test-selection interface with:

All accepted impl selectors:

```sh
sh ci/impl-toolchain.sh impls
```

Impl selectors whose runner tools are currently on `PATH` (an inventory,
not permission to reduce requested test coverage):

```sh
sh ci/impl-toolchain.sh installed-impls
```

## Execute in an exact environment

```sh
sh ci/impl-toolchain.sh toolchain-verify python
sh ci/impl-toolchain.sh toolchain-exec python -- python3 --version
```

An installer cannot change the calling shell's environment. Run checks through
`toolchain-exec` so PATH, manager homes, native SDK settings, and nested mise
execution select the same tools. It preserves the working directory and does
not install tools while executing. Repository build/check commands also pass
`--core` to verify and activate the general helper pool; plain tool witnesses
and the Windows Rust-only route do not need it. On Windows the Python executable
is `python`.

To select another tracked exact environment, pass its ID to both setup and
execution:

```sh
sh ci/impl-toolchain.sh toolchain-environments
sh ci/install-target-language-toolchains.sh python --environment ENVIRONMENT --plan
sh ci/install-target-language-toolchains.sh python --environment ENVIRONMENT
sh ci/impl-toolchain.sh toolchain-exec python --environment ENVIRONMENT -- python3 --version
```

Replace `ENVIRONMENT` with a listed ID. Separate processes can select different
installed environments concurrently; installing another environment does not
change a running command's selection.

## Python for the Python backend runner

The Python backend's test runner imports emitted packages with the local
Python interpreter: `python3` on Linux/macOS, `python` on Windows.
`mise.toml` pins Python. Core development and Python-backend execution reuse
that project-owned installation through `toolchain-exec`.

GHCup is an explicit delegated route: its child GHC version is pinned in
`tools/toolchains/ghcup.toml`, and `toolchain-exec haskell -- COMMAND` supplies
that compiler's bin directory. No `~/.ghcup/bin` profile edit is needed.

## Warm caches

The dev container pre-warms build caches. On a local checkout you can
reuse the container lifecycle scripts:

```sh
sh ci/impl-toolchain.sh toolchain-exec js,ts,python,rust --core -- sh .devcontainer/on-create.sh
sh ci/impl-toolchain.sh toolchain-exec js,ts,python,rust --core -- sh .devcontainer/update-content.sh
```

`on-create.sh` fetches Cargo and npm dependencies. `update-content.sh`
builds the Rust test binaries used by the checks. Both scripts are
POSIX sh, guarded, and idempotent.

## Build and check

Run a smoke build and the baseline local gate:

```sh
sh ci/impl-toolchain.sh toolchain-exec rust --core -- sh ci/cargo.sh build --manifest-path kio-rs/Cargo.toml
sh ci/install-target-language-toolchains.sh --supported-set
sh ci/impl-toolchain.sh toolchain-exec js,ts,python,rust,go,java,swift,haskell --core -- sh ci/all.sh SAMPLE_IMPL
```

The full baseline gate above is the Linux route and needs all eight languages
plus the core helpers. Native macOS/Windows jobs run their declared portability
checks instead. `SAMPLE_IMPL` runs one applicable implementation per case;
`FULL_IMPL_MATRIX` runs the full matrix, and a comma-separated impl list
restricts the run. See [`TESTING.md`](TESTING.md) § Local iteration for
the selector semantics.

For focused backend validation, prepare the selected languages and use the
bounded checks described in TESTING.md. A partial implementation
list does not remove the broader gate's other tool requirements.

For repeated Rust-heavy builds you can optionally wire in the
[sccache](https://github.com/mozilla/sccache) compiler cache — mise
provisions the binary, but nothing turns it on by default. Casual
contributors can skip it. To enable it, export the wrapper and pre-start
the daemon once per shell session:

```sh
export RUSTC_WRAPPER=sccache
export SCCACHE_IDLE_TIMEOUT=0
sh ci/impl-toolchain.sh toolchain-exec rust --core -- sccache --dist-status >/dev/null
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

- A direct account mise command says the config is untrusted: trust this
  checkout before using that account session. Project setup and execution use
  their verified controlled configuration.
- A target-language executable is missing or has the wrong version: rerun the
  selected installer and `toolchain-verify`, then launch through
  `toolchain-exec`. Do not fall back to an OS compiler or interpreter.
- `ghc` is missing: install `haskell` and execute through
  `toolchain-exec haskell`; a general mise shim does not select child GHC.
- Swift or Haskell backend runs fail on missing shared libraries: run
  `sh ci/impl-toolchain.sh install <impls>` for the impl list you are
  testing. That routes Linux system packages through mise bootstrap.
- A Cargo-backed mise tool takes a while to install: that is expected
  when mise's `cargo:` backend compiles the CLI from source.
