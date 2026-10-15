---
name: install-dev-env
description: Provision a checkout's core Kio development environment and explicitly selected target-language toolchains, warm caches, and verify setup; mirrors INSTALL.md
allowed-tools: Read, Grep, Glob, Bash, Edit, Write
---

# Install the development environment

Automate [INSTALL.md](../../../INSTALL.md). Read it and
[local-tools.md](../../topics/local-tools.md) before provisioning; they own
the human setup procedure and tool/storage policy. Keep this workflow paired
with them. [audit-install](../audit-install/SKILL.md) checks the pairing.

## Select the setup scope

Inspect existing tools and the requested checks before installing. Linux/macOS
core setup provides Node.js, TypeScript, Python, and Rust plus general helper
CLIs; it does not implicitly install every target language. Windows core setup
provides the supported Rust/compiler development route. Select only the additional
languages the task needs. Full Linux `ci/all.sh SAMPLE_IMPL` needs all eight
languages even though each case executes one implementation. Do not choose an
installed subset to evade requested coverage.

```sh
sh ci/install-target-language-toolchains.sh
sh ci/install-target-language-toolchains.sh --list
```

Plan inspection needs the pinned mise. Bootstrap only that verified manager if
absent; the plan operation itself does not download target-language tools:

```sh
sh ci/impl-toolchain.sh toolchain-bootstrap
sh ci/install-target-language-toolchains.sh swift haskell --plan
```

No arguments display help. `--supported-set` selects the supported development
toolchains on this OS; `--portability-set` selects the smaller set, including
on Linux. The native macOS/Windows supported sets match their GitHub jobs;
explicit other-language requests warn that setup is best-effort. Do not infer
complete POSIX suite support from a native portability job.

## Provision

Follow INSTALL.md's native prerequisites, including the C/C++ compiler/linker
environment on Linux, Xcode's tools/SDK on macOS, and Git Bash/MSYS plus the
MSVC C++ build tools/Windows SDK on Windows. The installer bootstraps
the verified pinned mise itself; no account installation/profile edit is required.

```sh
sh ci/impl-toolchain.sh install-core
sh ci/install-target-language-toolchains.sh swift haskell
```

The second command illustrates explicitly requested extras, not a standing
instruction to install them. For a full Linux environment use
`sh ci/install-target-language-toolchains.sh --supported-set` after core setup.
Linux system libraries route through mise bootstrap and may need administrative
privileges.
On macOS, core setup's Python helper dependency is best-effort and warns before
setup; use `--portability-set` for the supported native target-tool subset alone.

Exact tool requests/download identities live in native mise config/lock files;
delegated manager/child pins live under `tools/toolchains/`. Project language
tools and manager stores use `.toolchains/`; helpers use ordinary mise storage.
For Swift's native archive/signature route and Linux GnuPG prerequisite, follow
[INSTALL.md's ownership exception](../../../INSTALL.md#tool-ownership-and-storage).
Use an explicitly selected absolute `KIO_TOOLCHAINS_ROOT` consistently when
sharing a store. Preserve account defaults, shell profiles, `HOME`, Cargo
configuration/credentials, and configured compiler caches.

For a named exact environment, pass the same `--environment ID` to installation
and execution; `toolchain-environments` lists IDs.

Optional Cargo-backed report/audit tools remain opt-in:

```sh
sh ci/impl-toolchain.sh install-report-tools
```

This also prepares pinned fuzz Rust. Run stable report/audit commands through
`toolchain-exec rust --environment optional --reports -- COMMAND`; the fuzz wrapper selects the fuzz
environment. Optional tools stay outside ordinary core activation.

## Warm and verify

An installer cannot change its parent shell. Execute through the shared owner;
`--core` activates and verifies the separately stored general helpers as well:

```sh
sh ci/impl-toolchain.sh toolchain-exec js,ts,python,rust --core -- sh .devcontainer/on-create.sh
sh ci/impl-toolchain.sh toolchain-exec js,ts,python,rust --core -- sh .devcontainer/update-content.sh
sh ci/impl-toolchain.sh toolchain-exec rust --core -- sh ci/cargo.sh build --manifest-path kio-rs/Cargo.toml
```

These cache-warming examples apply to Linux/macOS and are optional. Windows's
Rust route uses target-only execution without the general helper pool. Before
builds follow AGENTS.md's configured compiler-cache readiness rule. Choose gating
coverage through [local-ci.md](../../topics/local-ci.md); after full Linux setup
the baseline is:

```sh
sh ci/impl-toolchain.sh toolchain-exec js,ts,python,rust,go,java,swift,haskell --core -- sh ci/all.sh SAMPLE_IMPL
```

Execution verifies exact versions/origins and does not install missing tools.
Missing/wrong tools are setup failures; do not fall back to account compilers,
rewrite goldens, skip required checks, or change language contracts.

## Close gaps and report

Repair missing-tool provisioning only within the requested setup scope and
established ownership. Add general helpers to mise, direct target tools to
native mise config/locks, and delegated child requests to their existing handler
manifest. A changed procedure updates INSTALL.md and this skill together.
Do not install tools to conceal code failures or unavailable network services.

Route reproducible setup friction to INSTALL.md, agent mechanics to local-tools,
and container lifecycle changes to their scripts. Apply the
[no-leak portability filter](../../topics/no-leak.md) before documenting it;
machine-specific state belongs in the report. Report installed/reused tools,
selected environments and platforms, verification results, remaining prerequisites,
and failures outside provisioning scope.
