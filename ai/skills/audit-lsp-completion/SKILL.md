---
name: audit-lsp-completion
description: Audit exact completion eligibility, binding freshness, and evidence across Kio LSP, terminal REPL, and browser completion.
allowed-tools: Read, Grep, Glob, Bash
---

# Completion audit

Anchor: [`language-surface-tooling.md` § Completion currency](../../topics/language-surface-tooling.md#completion-currency). Audit completion, not unrelated LSP requests or syntax highlighting.

**Default: read-only static audit.** Inspect source, test assertions, and existing execution receipts; do not build or launch servers, terminals, browsers, or integration harnesses. Report unexecuted coverage as unverified. Protocol execution is an explicit opt-in or validation of an authorized fix, scoped under § Execution below.

## Authorities and implementation map

Read AGENTS.md and the anchor topic. Establish eligibility from the effective authorized contract, including:

- [`specs/cli.md`](../../../specs/cli.md) § `kio lsp` and § `kio repl`: candidates, item shape, document identity, matching, replacement ranges, response completeness, and menu presentation.
- [`specs/grammar.md`](../../../specs/grammar.md), applicable semantic sections of [`specs/language.md`](../../../specs/language.md), and [`specs/style.md`](../../../specs/style.md): admitted forms and canonical spellings, not merely tokens the parser recognizes for diagnostics.
- [`specs/package.md`](../../../specs/package.md): file kinds, package/dependency/lock fields, source alternatives, host roles, build ordering, docs fields, and per-target keys.
- [`specs/versioning.md`](../../../specs/versioning.md): signature headers, version-local declarations, ordered change sections, and exact declaration references.
- The backend inventory and applicable target configuration sections under [`specs/backends/`](../../../specs/backends/), including each target's Output layout and Rust's Thread safety; [`specs/prime.md`](../../../specs/prime.md) and the CLI build contract for `kio-prime`. Enumerate every dispatched target and all of its key/value authorities, not just one host backend. Compare these with the shared target catalog and actual validation/dispatch in [`cmd/build.rs`](../../../kio-rs/src/cmd/build.rs); implementation metadata is evidence, not permission to expand the contract.

Use [`file_kind.rs`](../../../kio-rs/src/file_kind.rs) to check routing across regular modules, `.pkg.kio`, `.dep.kio`, `.lock.kio`, and `.sig.kio`. Rebuild the actual call graph from [`lsp/completion.rs`](../../../kio-rs/src/lsp/completion.rs), LSP routing/freshness in [`lsp/mod.rs`](../../../kio-rs/src/lsp/mod.rs), [`scope_walk.rs`](../../../kio-rs/src/scope_walk.rs), and their parser/context/provider dependencies. Follow the same shared result through [`repl_core/completion.rs`](../../../kio-rs/src/repl_core/completion.rs), [`repl/completion.rs`](../../../kio-rs/src/repl/completion.rs), [`kio-repl-wasm/src/lib.rs`](../../../kio-repl-wasm/src/lib.rs), and [`ReplIsland.vue`](../../../website/.vitepress/theme/components/ReplIsland.vue). Shared code does not by itself prove correct client routing or presentation.

## Eligibility and identity

Build a compact context matrix: file kind or REPL occurrence, grammar position/state, governing authority, eligible family/set, producer, and evidence. Discover contexts from both the contract and every candidate producer; missing providers and implementation-only choices remain visible. Distinguish lexical names, closed named vocabularies, and open values such as user paths. Required punctuation alone is not a named vocabulary. Target IDs are the closed dispatched inventory; output paths are not.

For closed vocabularies, compare exact sets in both directions, not a few `contains` assertions. Cover declaration leads/modifiers, import selections, member/annotation/control continuations, package fields and target IDs/keys, dependency source alternatives, lock fields, and signature change states. Exercise absent, already-written, mutually exclusive, reordered, partial, and recovered states. Preserve legal repetitions while retracting forbidden duplicates. An offered atom needs at least one otherwise-valid accepted continuation; a parser's reserved-spelling error branch is not such a witness. Report conflicting authorities instead of choosing whichever oracle agrees with the provider.

For identifier eligibility, verify exact lexical/source-order scope, namespace and visibility: explicit selective/qualified imports and intrinsics, ordinary and recursive function/type heads, generated label heads, type parameters, lambda/function parameters, sequential/destructuring/row-let locals, and shadowing. Qualified candidates must follow the selected module or nominal identity; same-leaf names, unrelated loaded modules, and private foreign declarations supply no candidates or enrichment. An unavailable selected edge yields only the authenticated subset with truthful incompleteness, not a broad fallback pool.

Check grammar-owned suppression at comments, literals, and binder introductions, with ordinary-name inverses and incomplete-source controls. Check type/value/label/operator routing, type/value naming namespaces versus command-discovery matching, and the exact whole-token replacement range, including multicharacter operators and UTF-8/UTF-16 client coordinates. Separate the eligible set from prefix matching/ranking and bounded visible menus; no eligible match may disappear through an undisclosed response cap.

Trace each candidate's binding identity and source version into optional detail/documentation. A label-only rescan, wrong namespace, stale typed shard, or another same-spelled provider cannot enrich it. Inspect addition/deletion/rename, shadow/unshadow, import retargeting, provider-only edits, incomplete intermediate versions, disk fallback after close, reopen, and REPL reload. Scheduling analysis is not proof that its result describes the requested version.

## Evidence at actual boundaries

For each load-bearing context and distinct occurrence, record existing or missing evidence for exact set equality, applicable negative choices, insertion, and edit/retraction. Insertion tests apply the actual returned edit and validate an otherwise-valid continuation; matching an edit string or accepting a different error is insufficient. Retraction tests change the governing source/state and assert the new exact set. Include provider freshness, wrong-identity and namespace inverses, and replacement/item metadata (`kind`, details, documentation, `isIncomplete`) where applicable.

- **LSP:** [`tests/lsp_smoke.rs`](../../../kio-rs/tests/lsp_smoke.rs) and its [test modules](../../../kio-rs/tests/lsp_smoke/) drive the real JSON-RPC server. Collector tests do not prove file-kind/URI routing, overlays, lifecycle, or encoded edits.
- **Terminal:** inspect shared-core tests and the actual interactive completion adapter/menu. [`tests/repl_smoke.rs`](../../../kio-rs/tests/repl_smoke.rs) uses pipes and does not prove keyboard completion. Menu claims need real interactive input, selection, dismissal, and access to matches beyond the first visible portion.
- **Browser/wasm:** inspect the public wasm `complete` entry and browser consumer separately. A native wrapper test, wasm build, or website asset check is not browser execution. Runtime claims need actual browser completion/replacement and keyboard/menu observations against the built wrapper.

A green suite only covers its executed assertions. Missing families, skipped platforms, discarded candidates, unvisited fixtures, or normalized-away mismatches are findings, not acceptance evidence. For a fix, preserve a causal failure against the unfixed behavior and the corresponding corrected result.

## Execution

Only for explicitly requested execution or scoped validation of an authorized fix, read [`local-ci.md`](../../topics/local-ci.md) and follow AGENTS.md's Cargo/cache/temporary-storage rules. Choose the smallest real boundary that tests the claim. The LSP orchestrator `sh ci/checks/orchestrators/lsp-tests.sh` runs the complete LSP suite and has no selector arguments; a focused real-server case uses `sh ../ci/cargo.sh test --test lsp_smoke TEST_NAME -- --exact` from `kio-rs/`, with a discovered full test name. Confirm a nonzero executed count. Inspect the existing terminal/browser entry points and their actual coverage before execution. Do not assume nonexistent keyboard-test commands or treat unexecuted tests or build success as runtime proof; an authorized fix may add a focused interactive test.

Do not launch the umbrella audit, broad integration gate, or full protocol suite merely because this static skill was invoked. Broaden only when requested or justified by the authorized fix's affected boundaries. Missing tools/platforms are explicit evidence gaps.

## Report

Report contract conflicts, missing/invalid candidates, identity/freshness errors, replacement/item/presentation defects, and evidence gaps. Cite authority, source location, concrete cursor/source state, expected versus actual set or edit, and the exact observed receipt where available. Distinguish inspected source, executed behavior, and unverified cells. Completion review does not clear unrelated diagnostics, highlighting, or overall implementation acceptance.

For a fix directive, follow [`audit-fix-mode.md`](../../topics/audit-fix-mode.md); a finding does not expand authority or silently change eligibility.
