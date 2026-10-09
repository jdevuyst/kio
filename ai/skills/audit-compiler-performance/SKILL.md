---
name: audit-compiler-performance
description: Audit the compiler and compiler-producing CI performance architecture — native resource scheduling, parallel fan-out, lazy parsing, semantic on-disk caches, LSP responsiveness, backend emission, generated execution and recursive host-stack scaling, and timing probes — for regressions as features land
allowed-tools: Read, Grep, Glob, Bash
---

# Compiler-performance audit

Audit whether the compiler still preserves its performance architecture, anchored in [`ai/topics/implementation.md` § Compiler performance architecture](../../topics/implementation.md#compiler-performance-architecture). This is not a microbenchmark pass and does not declare the compiler "fast enough" by wall-clock threshold. It checks for regressions in the design commitments Kio relies on:

- independent work fans out through the `maybe_par_iter!` family under the default `parallel` feature;
- source bodies are parsed lazily until a phase needs them;
- shared analysis results and indexes are kept in memory for LSP requests;
- `kio check` and `kio build` use the available Kio-semantic on-disk caches;
- LSP work is debounced, cancellable, backgrounded, and able to serve stale/focused results instead of blocking every request on full analysis;
- timing/cache probes remain available so performance regressions can be diagnosed.

Read these files before starting:

- `kio-rs/Cargo.toml`
- `kio-rs/src/par.rs`
- `kio-rs/src/package_collection.rs`
- `kio-rs/src/cmd/check.rs`
- `kio-rs/src/cmd/build.rs`
- `kio-rs/src/cmd/test.rs`
- `kio-rs/src/cache/mod.rs`
- `kio-rs/src/cache/equiv.rs`
- `kio-rs/src/cache/package_check.rs`
- `kio-rs/src/cache/typed.rs`
- `kio-rs/src/cache/enriched.rs`
- `kio-rs/src/cache/emit.rs`
- `kio-rs/src/cache/artifact.rs`
- `kio-rs/src/pass/typecheck_full.rs`
- `kio-rs/src/pass/typecheck_core/modules.rs`
- `kio-rs/src/lsp/state.rs`
- `kio-rs/src/lsp/worker.rs`
- `kio-rs/src/lsp/snapshot.rs`
- `kio-rs/src/lsp/cancel.rs`
- `kio-rs/src/backends/mod.rs`
- `kio-rs/src/timing.rs`
- `ci/infra/kio-ci-scheduler-rs/src/available_parallelism.rs`
- `ci/infra/kio-ci-scheduler-rs/src/held_resources.rs`
- `ci/infra/kio-ci-scheduler-rs/src/resource_admission.rs`
- `ci/infra/kio-ci-scheduler-rs/src/compiler_admission.rs`
- `ci/infra/kio-ci-scheduler-rs/src/compiler_feedback.rs`
- `ci/infra/kio-ci-scheduler-rs/src/compiler_feedback/host.rs`
- `ci/infra/kio-ci-scheduler-rs/src/compiler_trace.rs`
- `ci/infra/kio-ci-scheduler-rs/src/process_supervisor.rs`
- `ci/infra/kio-ci-scheduler-rs/src/self_test.rs`
- `ci/infra/kio-ci-scheduler-rs/bootstrap.sh`
- `ci/schedule.sh`
- `ci/cargo.sh`
- `ai/topics/implementation.md`

If the user gave a fix directive, read [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) before editing.

## 1. Inventory Freshness

Read [`ai/topics/implementation.md` § Compiler performance architecture](../../topics/implementation.md#compiler-performance-architecture). Use its table as the single inventory of substantial mechanisms; this skill owns the checks, and [evidence/](../../../evidence/README.md) owns completed measurements and arguments.

Check that the inventory names every substantial compiler-performance mechanism in the codebase:

```sh
git grep -nE 'cache|Cache|memo|Memo|interner|Interner|lazy|Lazy|debounce|focused|snapshot|CancellationToken|maybe_par_iter|par_iter|frontend-timing|lsp-timing|KIO_DEBUG_(TIMING|.*TIMING|.*CACHE|CI_SCHEDULER_TRACE|SAMPLE_IMPL_SEED|TEST_RUNNER_COMPILER_OBSERVER)|compiler_observer|available_parallelism|AdmissionEvaluation|ClaimStore|readiness_hook|JobObject' -- kio-rs/src ci/infra/kio-ci-scheduler-rs/src ci/infra/kio-test-runner-rs/src ci/all.sh ci/run-tests.sh
git grep -nE 'build_corpus_tool_binary|kio-corpus-tools' -- \
  ci/checks/orchestrators ai/topics/implementation.md
```

Also inspect backend preparation, emitted runtime/conversion owners and measured
`ARTIFACT_SHAPE` guards: keyword searches alone miss statement lowering,
allocation avoidance and generated-code sharing.

Classify each hit:

- **Registered mechanism** — covered by one row of the inventory and by a later audit section.
- **Implementation detail** — local helper inside a registered mechanism; no separate row needed.
- **New unregistered mechanism** — a distinct cache, memo table, lazy/deferred path, fan-out scheduler, background worker, cancellation/freshness mechanism, or performance probe not named in the inventory.

Findings:

- **Unregistered optimization** — a distinct performance mechanism exists in code but not in the inventory.
- **Stale inventory row** — a row names files or behavior that no longer exists.
- **Audit gap** — the inventory names a mechanism that the skill does not check.

If a change adds only another instance of an existing class, extend the existing row only when it adds a new anchor or changes the hot path materially. If it adds a new class of optimization, add a row and a matching audit check in the same change.

## 2. Parallel Fan-Out

Confirm the default feature set still includes `parallel`, `parallel` still depends on rayon, and `src/par.rs` remains the central shim from `maybe_par_iter!` / `maybe_par_iter_mut!` / `maybe_into_par_iter!` to rayon or sequential iterators.

```sh
grep -nE 'default = .*parallel|parallel = .*rayon' kio-rs/Cargo.toml
grep -nE 'macro_rules! maybe_(par_iter|par_iter_mut|into_par_iter)' kio-rs/src/par.rs
git grep -nE '\b(maybe_par_iter|maybe_par_iter_mut|maybe_into_par_iter|par_iter|into_par_iter)\b' -- kio-rs/src
```

Then inspect likely fan-out regressions:

```sh
git grep -nE '\.iter\(\)\.map|for .* in .*\.iter\(\)|for .* in &' -- \
  kio-rs/src/cmd kio-rs/src/pass kio-rs/src/backends kio-rs/src/kiodoc |
  grep -vE 'tests|sort|BTreeMap|BTreeSet|diagnostic|Display|fmt'
```

Classify each hit. Sequential code is fine when it is order-dependent, tiny fixed-size bookkeeping, post-processing of already-parallel results, or required to preserve deterministic output. A finding requires an independent per-package, per-module, per-target, per-doc-snippet, or per-file body pass that was moved to a serial loop without a concrete ordering dependency.

Specific fan-outs that must remain parallel when `parallel` is enabled:

- workspace package levels in `cmd/check.rs`;
- lazy body forcing and module typecheck inside a package level;
- `kio build` per-target dispatch in `cmd/build.rs`;
- backend package lowering/emission entry points documented in `backends/mod.rs`;
- parser / formatter / Kiodoc bulk work where each file or snippet is independent.

User-elaborator evaluator-artifact construction is the narrow exception. Inspect
`UserElaboratorArtifactTypecheck` in `pass/typecheck_full.rs`: its private
deferred-discharge and package-validation entry points must accept no execution
policy and must share the one `Sequential` policy. Ordinary package, module,
and user-elaborator-template checking must still default to `AllowParallel`,
permitting package-module, module-function, and sufficiently wide template-batch
fan-out. The artifact package entry point propagates the sequential policy
through both package-module and module-function checking.

Findings:

- **Lost fan-out** — an embarrassingly-parallel pass now runs serially.
- **Bypassed shim** — new compiler fan-out uses rayon directly without a clear reason, or adds a second parallel abstraction instead of the `maybe_par_iter!` family.
- **Non-deterministic fan-out** — parallel output collection now depends on worker completion order rather than stable source/package/target order.
- **Leaky artifact policy** — an evaluator-artifact caller can choose its execution policy, the two artifact entry points no longer share the one sequential policy, or `Sequential` becomes the default for an ordinary entry point.

## 3. Lazy Parse And Forced Work

Verify package walking still uses `parse_lazy` for regular modules and stores `ParsedPackage::lazy_modules`; eager `parse` should not become the default for all module bodies.

```sh
grep -nE 'parse_lazy|lazy_modules|force_all|force_parsed_package_bodies|force_and_lower_module' \
  kio-rs/src/package_collection.rs kio-rs/src/cmd/check.rs kio-rs/src/pass/parser/mod.rs kio-rs/src/pass/parser/core.rs
```

Classify:

- **Lazy parse preserved** — header/package surface is parsed first; bodies are forced only for modules that need lowering/typecheck or for whole-package paths that explicitly need all bodies.
- **Eager parse regression** — a workflow that could inspect headers, package surfaces, syntax-level LSP data, or cache metadata now forces every body.
- **Focused-work regression** — typed-module cache hits or focused LSP requests still force sibling module bodies without a dependency reason.

## 4. On-Disk Cache Layers

Inventory all Kio-semantic caches and confirm they still participate in the intended command path:

```sh
grep -nE 'PackageCheckCache|TypedCacheState|TypedModuleCache|package_check|typed_hits|typed_misses|skip_ok' kio-rs/src/cmd/check.rs
grep -nE 'EquivCache|EquivCacheKeyPrelude|write_stable_ast_framed|StableAstSerializer|maybe_par_iter' \
  kio-rs/src/cmd/test.rs kio-rs/src/cache/equiv.rs
grep -R -nE 'resolve_from_cache_field|recover_and_optimize_package_cached|lower_.*_cached|ArtifactCache|EmitCache|EnrichedCache|restore|store' kio-rs/src/cmd/build.rs kio-rs/src/backends
grep -nE 'CACHE_SCHEMA_VERSION|FORMAT_VERSION|SCHEMA_TAG|COMPILER_CACHE_ID|postcard|rename|\\.gitignore|log_probe' \
  kio-rs/src/cache/package_check.rs \
  kio-rs/src/cache/equiv.rs \
  kio-rs/src/cache/typed.rs \
  kio-rs/src/cache/enriched.rs \
  kio-rs/src/cache/emit.rs \
  kio-rs/src/cache/artifact.rs
```

Check the command semantics:

- `kio check` passes `skip_ok = true` and can skip warm package checks via `PackageCheckCache`.
- `kio build` passes `skip_ok = false` so it always has typed packages for codegen, but still benefits from typed-module, enriched-IR, emit, and artifact caches.
- Cached `kio test` constructs one `EquivCacheKeyPrelude` before the parallel per-`equiv` fan-out. The body-free evaluator package structure (package file, module paths and imports, non-function declarations, and function headers), primitive environment, and newtype inputs stream directly into a fixed-size digest. Each per-`equiv` key adds its substituted terms and the owner-aware transitive closure of reachable function bodies. Caller-provided maps are rejected, and admitted map-shaped registry state is projected into deterministically sorted sequences.
- `--no-cache` / `cache ();` disable reads and writes for Kio-semantic caches without changing behavior; disabled families also bypass cache-only source hashing, key construction, rendering, and serialization while preserving semantic validation and the ordinary uncached output path.
- Cache keys include compiler identity, schema/version tags, pipeline/target profile, source fingerprints, dependency/upstream fingerprints, and target inputs as applicable.
- Cache writes are atomic or otherwise concurrency-safe; stale/malformed entries are misses, not panics.
- Cache hit/miss probes still exist (`KIO_DEBUG_*_CACHE` or `KIO_DEBUG_TIMING` frontend timing).

Also audit the exact nominal-scope/provider acceleration used by type and
transparent-alias resolution:

```sh
grep -nE 'TypeImportIndex|type_imports: Option<Arc|NominalProvider|BorrowedTargetCache|PersistentExact(Index|Set)' \
  kio-rs/src/pass/resolve.rs \
  kio-rs/src/pass/typecheck_core/aliases.rs \
  kio-rs/src/pass/typecheck_core/persistent_exact.rs
grep -nE 'scheduled_typed_module_entry|TopLevelScope::build' \
  kio-rs/src/cmd/check.rs \
  kio-rs/src/pass/substitute/mod.rs \
  kio-rs/src/prime/typer.rs
```

Confirm that modules without type-shaped selective or qualified type-import
edges allocate no `TypeImportIndex`; clones share a present index through one
`Arc`; lowercase value imports add no type entry; first-written edge/span and
debug/serde/cache-fingerprint behavior remain unchanged; and every
phase-transformed `ModuleEntry` rebuilds its scope from the transformed module.
`NominalProvider` caches only already-authorized exact declaration/import-edge
results and fail-closed negative owner handles for its call lifetime. It must
never search ambient declarations or make resolution depend on an unrelated
same-spelled declaration. Alias binder/alpha/frontier contexts use structurally
shared exact nodes rather than cloning cumulative maps, while canonical
non-alias roots activate no materializer arena.

Audit shared annotation/header planning and its persistent lexical view:

```sh
grep -nE 'AnnotationPlan|LambdaHeaderPlan|PlanningLexicalView|PromotedPlanningLexicalView|PersistentExactNameMap|AliasSourceOccurrenceTransport|SourceOccurrenceTransportSink' \
  kio-rs/src/pass/typecheck_core/annotation_plan.rs \
  kio-rs/src/pass/typecheck_core/aliases.rs \
  kio-rs/src/pass/typecheck_core/apply.rs \
  kio-rs/src/prime/typer.rs
```

Confirm that closed plans consumed on the stack borrow their exact lexical
slice and allocate no occurrence index, output transport, goal owner,
subscriber, or table.
A plan that must move promotes the ambient exact-name prefix once; sibling
plans share that persistent root in O(1) and extend only their narrow lexical
child, so D-deep scope plus K siblings performs O(D+K) name work rather than
cumulative rescans or clones. Direct, retained, and Prime policies consume one
authoritative plan instead of replanning. An open direct non-alias annotation
indexes only path/`Infer` source frontiers and emitted-`Infer` order, not every
structural node; output ids, Continue/Graft/root contributions, and their
sparse DAG begin only at an exact transparent-alias redirect. Direct
structural-binder provenance and output-binder provenance share one binder-only
arena. Source/output/binder ids use the zero niche for compact optional
storage, and each source keeps only a retained-route tag plus binder ids rather
than a per-node output vector. No second source-parent DAG is retained beside
the output `Continue` relation.
The transport root is move-only rather than a deep-clonable value. Canonical
closed non-alias annotations allocate no occurrence or transport state.

Findings:

- **Cache bypass** — a hot `kio build`, `kio check`, typecheck, elaborator, emit, or artifact path recomputes work that an existing cache layer is meant to cover.
- **Under-keyed cache** — a key omits source, dependency, target-profile, pipeline, feature, schema, or compiler identity input that can change the cached output.
- **Fragile cache read** — stale/corrupt cache contents can panic or surface as a user-visible compile error instead of falling through to recomputation.
- **Unsafe cache write** — concurrent workers can corrupt the same key or observe partially-written bytes.
- **No probe** — a cache layer has no practical hit/miss or timing signal.

## 5. LSP Responsiveness

Confirm the LSP still avoids synchronous full analysis on every edit/request:

```sh
grep -nE 'DEBOUNCE|Scheduler|spawn_with_debounce|schedule_immediate|run_debounce|run_worker|WorkPriority|FocusedAnalysis|CancellationToken|is_cancelled' \
  kio-rs/src/lsp/worker.rs kio-rs/src/lsp/cancel.rs
grep -nE 'OpenDocument|line_index|parsed_module|lazy_module|StoredAnalysis|FocusedAnalysisState|focused_by_uri|mark_focused_scheduled|focused_worker_result_is_obsolete|store_(focused_)?analysis|best_analysis|snapshot|covers|overlay_snapshot|pub fn close' \
  kio-rs/src/lsp/state.rs kio-rs/src/lsp/snapshot.rs kio-rs/src/lsp/mod.rs
grep -nE 'analyze_workspace_at_with_overlay_lsp_cancellable|analyze_module_at_with_overlay_lsp_cancellable|position_index|file_to_module|root_elaborations' \
  kio-rs/src/cmd/check.rs
git grep -nE 'analyze_workspace_at_with_overlay_lsp|analyze_module_at_with_overlay_lsp|schedule_immediate|best_analysis_for_file|with_overlay_lazy_module|with_overlay_parsed_module' -- kio-rs/src/lsp
```

Trace focused state through scheduling, publication, successful full-analysis
publication, and `didClose`. Focused work is scheduled only for an open
canonical document. Closing that document drops its shard and freshness
watermark and makes an in-flight focused result obsolete. A successful full
result advances a focused lifecycle only when its file appears in the analysis
and the full snapshot covers its current open-document version, so older
focused work cannot resurrect state. It drops only those shards whose snapshots
are no newer than the full result; failed full results and newer,
version-uncovered, or unrelated shards remain available. Focused-shard and
focused-watermark cardinality must remain bounded
by the number of open documents.

Classify:

- **Pass** — `didChange` work is debounced/backgrounded, typed foreground requests can schedule focused work, superseded work is cancellable, read-only handlers can serve a stored fresh/focused/stale-good analysis, syntax-only handlers use cached/lazy open-document parses, and focused shards and their freshness watermarks obey the bounded open-document lifecycle above.
- **Blocking regression** — hover, completion, definition, references, symbols, folding, formatting, or diagnostics now performs full workspace analysis synchronously on the main LSP loop without a freshness/latency argument.
- **Freshness regression** — stale worker results can overwrite fresher snapshots, a full result can discard a newer or unrelated focused shard, or diagnostics no longer carry overlay versions.
- **Retention regression** — focused-shard or focused-watermark cardinality can outgrow the open-document set, `didClose` can leave or resurrect focused state, or a successful full result retains a covered shard that is not newer than the full result.
- **Overlay regression** — analysis or formatting reads disk instead of open-buffer overlays for open documents.
- **Index reuse regression** — a request recomputes data that already lives in `LspAnalysis`, `StoredAnalysis`, focused shards, `OpenDocument` caches, or `Snapshot`.

## 6. Timing And Diagnosis Hooks

Performance work needs observable signals. Confirm timing probes still cover frontend and LSP analysis, and cache probes still cover the major cache layers.

```sh
sed -n '1,220p' kio-rs/src/timing.rs
grep -nE 'frontend-timing|lsp-timing|typed_hits|typed_misses|forced_modules|summary_levels|summary_max_width|total_ms|KIO_DEBUG_(TIMING|.*TIMING|.*CACHE)' \
  kio-rs/src/cmd/check.rs kio-rs/src/lsp/worker.rs kio-rs/src/cache/*.rs
grep -nE 'KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER|CompilerObserver' \
  ci/run-tests.sh ci/infra/kio-test-runner-rs/src/shared/compiler_observer.rs \
  ci/infra/kio-test-runner-rs/src/bin/kio-test-runner-{rust,go,java,haskell,swift}.rs
grep -nE 'kio-ci-compiler-admission-v3|KIO_DEBUG_CI_SCHEDULER_TRACE|adaptive-cap|program_basename_hex|create_new' \
  ci/infra/kio-ci-scheduler-rs/src/compiler_trace.rs \
  ci/infra/kio-ci-scheduler-rs/README.md \
  ai/topics/local-performance.md
```

Findings:

- **Blind hot path** — a substantial new frontend/build/LSP cache or scheduling path has no cache/timing probe and no way to distinguish hit, miss, cancelled, or stale-result behavior.
- **Misleading timing** — a timing line omits enough context that results cannot be compared across package, priority, scope, or cache state.

## 7. Tests And Guardrails

Look for tests or per-case checks pinning the performance architecture:

```sh
git grep -nE 'cache|typed_hits|typed_misses|package-check-cache|rlib-cache|focused|debounce|cancell|lazy|snapshot|best_analysis|parallel|deterministic' -- \
  kio-rs/src ci test-data TESTING.md
git grep -nE 'CompilerAdmission|acquire_for|compiler_admission' -- \
  ci/infra/kio-ci-scheduler-rs/src ci/infra/kio-test-runner-rs/src
grep -nE 'cargo_admission_is_fifo_and_capacity_bounded|active_work_barrier_blocks_every_new_admission|pending_work_barrier_blocks_normals_and_has_barrier_fifo|active_marker_wins_an_interrupted_pending_to_active_transition|unknown_live_lease_is_a_store_level_fail_closed_condition|no_persistent_ticket_counter_is_created_and_empty_queues_reuse_one|inherited_lease_blocks_reuse_until_the_descendant_exits' \
  ci/infra/kio-ci-scheduler-rs/src/resource_admission.rs
grep -nE 'native_available_parallelism_is_positive' \
  ci/infra/kio-ci-scheduler-rs/src/available_parallelism.rs
grep -nE 'held_resources_accept_only_canonical_work_cargo_compiler_order|malformed_held_resource_lists_are_rejected|already_held_resource_is_reused|requesting_earlier_resource_after_later_one_is_rejected|newly_acquired_resources_extend_the_canonical_marker' \
  ci/infra/kio-ci-scheduler-rs/src/held_resources.rs
grep -nE 'live_cpu_ceiling_constrains_feedback_and_fixed_capacity|lower_live_cpu_ceiling_blocks_a_fourth_producer|feedback_target_and_advertised_cpu_ceiling_gate_actual_admission|feedback_target_one_stops_new_admission|fixed_only_skips_feedback_and_non_head_reports_no_measured_target|fixed_four_still_admits_four_producers|typed_evaluation_drives_literal_store_outcomes|typed_policy_blockers_cover_every_reachable_queue_wait|smaller_fixed_cap_precedes_adaptive_feedback|store_level_unknown_live_wait_maps_to_the_typed_blocker|traced_wait_then_admission_records_the_same_transition|trace_setup_follows_ticket_publication_and_unlock|disabled_admission_creates_no_trace_file|malformed_trace_token_fails_before_admission|explicit_and_adaptive_caps_remain_hard_limits|omitted_capacity_is_adaptive_and_explicit_capacity_is_fixed|configured_readiness_hook_runs_before_the_lease_is_inherited|inherited_compiler_lease_reuses_established_readiness|reused_compiler_cli_skips_readiness_with_or_without_global_bypass|compiler_descendant_retains_lease_after_supervisor_and_parent_die' \
  ci/infra/kio-ci-scheduler-rs/src/compiler_admission.rs \
  ci/infra/kio-ci-scheduler-rs/src/lib.rs
grep -nE 'portable_trace_token_validation_rejects_paths_and_device_names|produced_lines_are_valid_v3_json_with_the_required_field_types|repeat_tickets_use_unique_process_acquisition_files|trace_records_generic_attribution_without_the_program_path|unchanged_waits_deduplicate_but_semantic_transitions_are_recorded' \
  ci/infra/kio-ci-scheduler-rs/src/compiler_trace.rs
grep -nE 'sustained_backlog_grows_and_pressure_only_lowers_future_target|no_backlog_busy_cpu_and_low_memory_do_not_grow|ceilings_bad_clocks_and_incomparable_counters_reset_or_clamp|shared_record_throttles_successive_heads_and_resets_idle_or_torn_state|unwritable_record_rejects_even_a_fresh_high_target|existing_store_lock_serializes_one_feedback_step_and_preserves_holders|linux_counter_units_and_guest_overlap_are_explicit|native_sample_is_valid_or_explicitly_unavailable' \
  ci/infra/kio-ci-scheduler-rs/src/compiler_feedback.rs \
  ci/infra/kio-ci-scheduler-rs/src/compiler_feedback/host.rs
grep -nE 'readiness_descendants_do_not_retain_scheduler_leases|waits_for_the_complete_process_tree|supervisor_death_terminates_the_complete_process_tree|readiness_descendants_break_away_from_the_owned_job|scheduler_job_nests_inside_an_outer_job|if !stdin_was_open|stdin\(Stdio::null\)' \
  ci/infra/kio-ci-scheduler-rs/src/process_supervisor.rs
grep -nE 'isolated_self_test_exercises_all_resources_and_supervised_stdio' \
  ci/infra/kio-ci-scheduler-rs/src/self_test.rs
grep -nE 'CI scheduler self-test|CI scheduler crate tests' .github/workflows/ci.yml
grep -nE 'prelude_digest_covers_non_package_shared_inputs|prelude_digest_excludes_only_ordinary_function_bodies_from_package|streamed_stable_ast_preserves_the_sanitized_logical_tree|stable_ast_digest_ignores_only_sanitized_fields|streamed_stable_ast_rejects_unprojected_map_inputs' \
  kio-rs/src/cache/equiv.rs
grep -nE 'exact_key_(header_)?rejects_a_(different_valid_module_body|swapped_valid_entry)|body_digest_rejects_(a_)?different_valid_(module|result|entry|utf8)|tree_digest_rejects_mutated_cached_output' \
  kio-rs/src/cache/typed.rs \
  kio-rs/src/cache/enriched.rs \
  kio-rs/src/cache/equiv.rs \
  kio-rs/src/cache/emit.rs \
  kio-rs/src/cache/artifact.rs \
  kio-rs/src/cache/package_check.rs \
  kio-rs/src/kiodoc/cache.rs
grep -nE 'tree_digest_is_independent_of_creation_order|tree_digest_distinguishes_renames_nesting_and_empty_directories|copy_dir_contents_rejects_(symlinks|special_files)|rejected_source_entry_cleans_the_temporary_directory|concurrent_same_key_publication_is_idempotent|nonreplacing_rename_accepts_a_valid_peer_publication|corrupt_destination_repair_accepts_a_peer_winning_the_retry|failed_repair_cleans_its_temporary_directory|repair_retry_rejects_an_invalid_peer|store_repairs_a_non_directory_destination' \
  kio-rs/src/cache/artifact.rs
grep -nE 'store_repairs_a_non_file_destination' \
  kio-rs/src/cache/equiv.rs \
  kio-rs/src/cache/emit.rs
grep -nE 'flat_file_publish_accepts_a_valid_same_key_peer|flat_file_repair_accepts_a_valid_peer_winning_the_retry|flat_file_repair_rejects_an_invalid_retry_peer_and_cleans_temp' \
  kio-rs/src/cache/mod.rs
grep -nE 'user_elaborator_artifact_entry_points_are_sequential|user_elaborator_artifact_pairs_are_analysis_scoped_singleflight|reused_structural_interner_does_not_reuse_package_content|imported_consumer_batches_share_one_exact_artifact_per_package_check|eager_result_inference_and_deferred_check_share_one_exact_artifact|staged_provider_helper_reconstructs_imported_surface_elaboration|package_versions_share_structural_types_but_not_content_memos_or_analysis_artifacts|revision_store_reuses_only_exact_versions_and_binds_each_scope|lsp_user_elaborator_memo_survives_repeated_same_snapshot_analysis|lsp_missing_focus_fallback_reuses_worker_memos|lsp_user_elaborator_memo_invalidates_after_provider_edit|user_elaborator_template_memo_replays_across_forced_modules|UserElaboratorArtifactTypecheck' \
  kio-rs/src/pass/typecheck_full.rs kio-rs/src/pass/typecheck_core/intern.rs kio-rs/src/cmd/check.rs kio-rs/tests/package_check_cache_skip.rs
grep -nE 'focused_retention_is_bounded_by_open_documents|closed_document_rejects_late_focused_result|reopened_document_rejects_a_result_from_its_previous_open_lifetime|repeated_did_open_starts_a_new_focused_lifetime|changed_canonical_identity_starts_a_fresh_focused_lifecycle|newer_same_file_focus_preserves_stale_good_and_supersedes_old_result|close_drops_focused_state_after_canonical_identity_drift|full_analysis_drops_only_covered_nonnewer_focused_shards|successful_full_analysis_rejects_an_older_inflight_focused_result|closed_file_foreground_request_schedules_full_analysis|reopened_document_does_not_publish_a_previous_lifetime_focused_result|failed_full_analysis_retains_focus_and_does_not_advance_its_watermark' \
  kio-rs/src/lsp/state.rs kio-rs/src/lsp/mod.rs
grep -nE 'top_level_scope_(common_header_is_one_word_over_declarations|uses_optional_shared_type_import_storage|indexes_written_type_import_edges)|nominal_provider_header_is_lazy_and_compact|canonical_non_alias_composite_skips_the_materializer_arena|alias_frontier_probes_do_not_snapshot_the_entire_rigid_scope|scheduled_typed_module_entry_indexes_the_transformed_module' \
  kio-rs/src/pass/resolve.rs \
  kio-rs/src/pass/typecheck_core/aliases.rs \
  kio-rs/src/cmd/check.rs
grep -nE 'borrowed_view_is_allocation_free_and_finds_the_innermost_shadow|one_promotion_is_shared_by_all_sibling_views|rejected_direct_forall_hole_is_classified_once_without_precommit_mutation|dropped_alias_occurrence_has_no_route_goal_or_rejection|direct_occurrences_stay_untransported_around_one_alias_branch|dropped_occurrence_under_a_direct_forall_has_no_binder_route|declined_immediate_lambda_transfers_the_exact_plan_without_goal_precommit|lowered_check_and_synth_routes_consume_the_shared_header_plan_once|ordinary_retained_and_expected_lambda_policies_consume_the_shared_plan|prime_check_synth_and_immediate_policies_consume_the_shared_header_plan' \
  kio-rs/src/pass/typecheck_core/annotation_plan.rs \
  kio-rs/src/pass/typecheck_core/aliases.rs \
  kio-rs/src/pass/typecheck_core/apply.rs \
  kio-rs/src/pass/typecheck_full/annotation_plan_public_tests.rs \
  kio-rs/src/prime/typer.rs
grep -nE 'empty_application_holes_use_direct_completed_type_equivalence|application_holes_keep_completed_type_solver|explicit_prime_arguments_keep_path_alias_literal_and_lambda_checks|explicit_prime_argument_mismatches_keep_the_argument_span' \
  kio-rs/src/pass/typecheck_core/apply.rs kio-rs/src/prime/typer.rs
grep -nE 'deep_publication_closes_payloads_without_rewalking_command_shape|noncanonical_retained_publication_validates_until_canonical_proof_is_minted|goal_bearing_retained_publication_validates_until_solution_then_reuses_proof|publication_reuses_a_canonicalized_function_scheme_alias_at_root|publication_preserves_child_rigids_while_parent_goals_close|goal_free_retained_publication_rejects_an_opposite_root_kind_before_reuse|goal_free_retained_publication_rejects_another_store_before_reuse|prepared_root_with_reused_publication_becomes_stale_before_commit|publication_failure_leaves_semantic_state_and_elaborations_untouched' \
  kio-rs/src/pass/typecheck_core/goals.rs kio-rs/src/pass/typecheck_full/publication.rs
test -f test-data/goldens/00_success/test_equiv_cache_warm_invalidation/run.sh
grep -nE 'function edit|private-helper edit|alias edit|qualified-alias/local edit' \
  test-data/goldens/00_success/test_equiv_cache_warm_invalidation/run.sh
grep -nE 'test_stable_corpus_tool_target|build_corpus_tool_binary|kio-corpus-tools' \
  ci/checks/repo-lint/schedule-entry-selftest.sh \
  ci/checks/orchestrators/lib/common.sh
grep -nE 'kio-lsp-cli\|kio-prime|profile.dev.package.kio-lang|expected-compiler-profile|changed compiler profile' \
  ci/checks/orchestrators/lib/common.sh \
  ci/checks/repo-lint/schedule-entry-selftest.sh
grep -nE 'observer_(is_outermost|fires_for_a_cache_miss)|uses_the_shared_outer_observer_shape|disabled_cache_still_observes|test_compiler_observer_readiness_and_lease' \
  ci/infra/kio-test-runner-rs/src ci/checks/repo-lint/schedule-entry-selftest.sh
grep -nE 'recursive_context_partition_scan_fires_linearly_at_the_production_seam|exact_recursive_context_bypasses_the_difference_scan' \
  kio-rs/src/sig/validate.rs
grep -nE 'canonical_type_rec_cyclic_components|TypeRecBoundNames|TYPE_REC_SCC_CANONICALIZATION_VISITS|first_extra_sorted_member' \
  kio-rs/src/pass/resolve.rs kio-rs/src/sig/validate.rs
grep -nE 'component_is_cyclic|record_type_rec_classification_(query|inspection)|cyclic_component_classification_' \
  kio-rs/src/pass/resolve.rs kio-rs/src/pass/resolve/type_rec_classification_tests.rs \
  kio-rs/src/pass/label_elab/mod.rs
```

Findings:

- **Missing regression guard** — a change modifies scheduler/cache/lazy/parallel architecture but adds no focused evidence at the boundary it affects: unit/mutation coverage for a private no-filesystem invariant, a cross-implementation golden for observable language/CLI/runtime behavior, or a backend-first `ARTIFACT_SHAPE` emission for a durable spec- or recorded-measurement-backed generated-file fact.
- **Benchmark-only guard** — the only validation is a local timing claim with no structural test, cache-probe assertion, or deterministic behavior check.
- **Scheduler bypass or split authority** — a native runner reaches an actual
  compiler without shared `CompilerAdmission`; shell code reimplements work,
  Cargo, or compiler queue policy; a command acquires resources outside
  `work -> cargo -> compiler` order; or a runner takes a permit before the
  per-key second cache probe so warm hits and same-key waiters consume compiler
  capacity. Cache-disabled, direct, and coexist compiler routes are in scope.
- **Hidden readiness special case** — the Rust scheduler matches `sccache` or
  another executable name instead of receiving a generic explicit hook, or the
  hook can retain Unix lease descriptors / a Windows Job.
- **Observer boundary gap** — a native runner compile route bypasses the shared
  opaque observer constructor; cache-disable disables observation; a version
  probe, cache hit/waiter, or runtime launch is observed; the observer enters a
  cache key; or readiness infers an observer from its basename instead of
  peeling the exact configured outer layer.
- **Platform-evidence gap** — policy tests diverge by OS, or Unix descriptor and
  Windows Job semantics are credited from cross-target compilation rather than
  native execution. The macOS and Windows jobs run the same native self-test
  and crate tests; cross-target checks remain compile evidence only.

End-to-end performance evidence must exercise the invocation agents are
actually instructed to use, resolved from `AGENTS.md` and
[`ai/topics/local-ci.md`](../../topics/local-ci.md) at measurement time. For the
ordinary broad gate, run `ci/all.sh SAMPLE_IMPL` with neither `--jobs` nor
`--compiler-jobs`; agent log-retention and supervision requirements still
apply. Fixed numeric overrides are useful for controlled attribution or a
deliberate constraint, but they earn no credit for default-path performance and
must not become a magic number in routine agent guidance. The production
default is responsible for selecting capacity automatically.

For a same-tree repeat or A/B comparison of a `SAMPLE_IMPL` broad gate, use one
canonical `KIO_DEBUG_SAMPLE_IMPL_SEED` value across every arm and retain the
logs. Require byte-identical `sample-impl-map:` lines for each matching corpus
and regular/direct-Prime/dynamic-Prime phase before attributing a timing
difference. That proves only implementation assignment: require identical
explicit `--case-seed`, `--gen-seed`, generated-case count, and case-coverage
policy too, then compare the printed selected-case blocks and `kio-gen:`
seed/count/Prime-mode records. Verify that the unset path still obtains
entropy, emits no mapping lines, and retains the ordinary invocation and
output; the probe must remain debug-only and must not become a public flag or
a substitute for automatic scheduler capacity.

## 8. Optimization Justification

Per AGENTS.md § Universal rules ("Optimizations are justified, not assumed"),
retained performance-only complexity must carry a recorded measured benefit and
causal firing coverage. Classify required correctness, stack safety and practical
scaling separately from optional speed techniques; apply
[Backend quality and implementation cost](../../topics/emit.md#backend-quality-and-implementation-cost)
to backend work. A known optional win may be declined for a concrete
implementation/review/maintenance cost without remeasuring it merely to prove
the same benefit. Required semantics and usable host APIs remain non-negotiable.

Follow the [inventory](../../topics/implementation.md#compiler-performance-architecture)
to discover relevant mechanisms. The checks below retain their invariants;
completed comparisons live in [compiler performance evidence](../../../evidence/compiler-performance.md)
and [backend performance evidence](../../../evidence/backend-performance.md).
Reuse evidence while its assumptions hold, not a new experiment per inventory
row. Reassess affected source, representation, workload or toolchain changes and
retain the measurement's stated limits.

- **Shared recursive-group step** (`pass/desugar/mod.rs`) — multi-member
  function groups share one ordinary private step function rather than copying
  every member body into every entry wrapper. Preserve per-entry state/loop
  construction, outer versus member-existential binders, source-name hygiene,
  retained expression identities, and ordinary textual Prime validation; the
  singleton path remains inline. [Recorded comparison and controls](../../../evidence/compiler-performance.md#shared-recursive-group-step). These are structural savings,
  not runtime acceptance: retain the all-backend quoted-source, polymorphic,
  visibility and deep-continuation goldens and measured runtime controls.

- **JS/TS prepared host roots** (`backends/js/emit.rs`) — reuse one native-function root per
  referenced exact host site inside its emitted module, without hoisting host
  lookup, type/value advancement or invocation. The shared JS runtime also
  serves TS. Check delayed nullary calls, staging, unused-site exclusion,
  exact owner identities, deterministic output and warm/cold emit-cache
  equality. Remeasure the incremental benefit when changing the callable
  representation rather than attributing a combined improvement to root reuse.
  [Measured comparisons and firing controls](../../../evidence/backend-performance.md#js-and-typescript-host-root-reuse)
  retain the limits of the roots-only evidence.

- **JS/TS static value-group extraction** (`backends/js/emit.rs`) —
  `wrap_js_signature_stages` uses the already-resolved source group to bind
  multiple parameters directly from the canonical product. The existing
  private domain parameter is a flat cursor, not mutated shared data. Read
  each spine edge once and give the final parameter the whole tail; do not
  recursively flatten a product-valued final parameter. Preserve zero/one
  parameter groups, executable type stages, source/native grouping, delayed
  calls and hygienic names. Avoid nested binding patterns that add a host
  parser depth limit. Keep generated-size bounds and real staged-call
  controls alongside private firing tests; do not extrapolate this target
  realization without inspecting applicable paths. The
  [binding measurements and controls](../../../evidence/backend-performance.md#js-and-typescript-signature-group-binding)
  establish a runtime trade-off, not a compiler-time or broad-gate win.

- **Python/Java direct canonical-product access** (`backends/{python,java}/runtime.*`) —
  bind each existing signature stage directly from its canonical domain,
  retaining the final product-valued tail, unary Unit, nullary groups,
  lexical environments and executable type stages. Keep exact staged-call,
  substituted-Unit, existential-projector and reentry golden coverage;
  private source guards are not execution evidence. Python single-slot
  projection must evaluate its target once, walk only the selected prefix
  and retain bounds errors, the whole final tail and nominal field access.
  Keep effect-order and record-field runtime goldens. The
  [guest-binding and projection evidence](../../../evidence/backend-performance.md#python-and-java-canonical-product-access)
  distinguishes allocation, private evaluator and application measurements;
  none establishes a whole-application or broad-gate speedup.

- **Typed runtime-plan storage**
  (`backends/{value_plan,serialized_runtime_ir}.rs`, Java emitter) —
  construct body plans only for consumers that execute typed stages, and intern
  them by the shared storage identity, omitting only diagnostic
  binder spans. Retain node/capability distinctions, exact source-use IDs,
  binder names/kinds, ambient binders, roots and nominal identities. Use one
  serializer for erased and typed execution, not divergent semantic walkers;
  erased consumers must neither serialize nor construct admission-only plans.
  Native type rendering remains an independent consumer of the common value
  planner. Check key/projection/scaling firing and absolute-byte plus growth
  bounds over all generated support, regardless of whether the target needs
  runtime plans. The
  [storage measurements](../../../evidence/backend-performance.md#typed-runtime-plan-storage)
  do not imply savings from absent plans or a runtime win from byte counts.

- **Shared literal Unit beta reduction** (`pass/optimize.rs`) — consume only
  a literal's exactly-one-empty-value stage. Preserve an explicit Unit
  argument through `Seq` before its body; retain returned closures and captures,
  and leave type or additional value stages intact. [Recorded comparison and controls](../../../evidence/compiler-performance.md#shared-literal-unit-beta-reduction). Retain both
  production firing and final emitted measurements rather than treating the
  exploratory rewrite as implementation evidence.

- **Settled fills normalization and checked-call replay** —
  `materialize_projected_type_slot` reuses one authenticated normalization of
  the same original slot within its immediate store/delta boundary; later
  writes still require fresh normalization. Snapshot reflection keeps its
  original two-pass path. The exact-representation/scope/error test in
  `apply/fills/tests/boundary_reuse_tests.rs` counts actual readiness probes.
  Full substitution must move owned stage arguments: the exact-tree/allocation
  test in `substitute/checked_call_tests.rs` fails if slice cloning returns.
  Restoring the duplicate slot probe or stage copying makes those guards fail.
  Full and Prime canonical
  slot transitions, UFCS, intrinsic/evaluator adapters, eta staging and rollback
  retain their focused guards. [Measured comparisons and limitations](../../../evidence/compiler-performance.md#settled-type-slots-and-checked-call-replay)
  retain the materialization and stage-assembly evidence.

- **Demand-initialized replay requalification** (`pass/substitute/mod.rs`,
  `pass/typecheck_full.rs`) — input replay constructs and seeds a fresh
  requalifier only at its first identity-canonical recorded type; capture
  replay initializes its separate requalifier at its first external capture.
  Preserve disabled evaluator contexts, eager whole-module alias injection,
  immutable module/elaboration inputs, lexical binders, exact generated names,
  and publication after each input. There is no cross-input mutable reuse.
  `replayed_input_without_canonical_types_skips_requalifier_initialization`
  and `template_replay_initializes_capture_requalifier_only_for_external_imports`
  count actual constructor calls through the production entries; restoring
  eager setup produces separate genuine failures. The latter also covers
  local-only and multiple external captures.
  `replayed_input_requalification_matches_eager_binding_boundaries` compares
  complete output and recorded bindings with eager construction across
  shadowed, repeated and unshadowed inputs, including alias/parameter collisions
  and one initialization for multiple canonical slots. Retain current-capture
  authority, value-before-template import reservation, disabled-eval and
  whole-module materialization controls.
  [Recorded comparison and controls](../../../evidence/compiler-performance.md#demand-initialized-replay-requalification).

- **Package-scoped signature identity index** (`pass/typecheck_core/modules.rs`, `pass/resolve.rs`) — a complete package-signature check shares one existing exact package-owned scope across its module checks. Every module still passes the original scope pointer assertions and signature checks with the same execution policy; individual-module checks and later package-check invocations create fresh scopes. The lazy identity-alias index cannot outlive or cross the invocation's exact package authority. [Recorded comparison and controls](../../../evidence/compiler-performance.md#package-scoped-signature-identity-index). Restore fresh scopes per module and require this unchanged test to fail; an alias-edge-only counter misses the wasted work. Require matched final-byte measurements with a bounded bridge configuration as well as exact signature output equality; retaining an independently growing bridge-selector axis does not establish declaration-only scaling.
- **Indexed fresh-signature origin slices** (`sig/validate.rs`, `sig/replay.rs`) — origin imports and final full-epoch semantic validation retain their existing authority. A candidate-module index and name-only dependency slices avoid rebuilding all declarations separately for origin resolution and identity qualification. [Recorded comparison and controls](../../../evidence/compiler-performance.md#indexed-fresh-signature-origin-slices). Require separate causal failures when the full name-only resolution view or the full qualification package is restored; a caller-side slice/fallback tally cannot detect an extra package construction. Restore the old per-module scan through that observed iterator and require its independent failure. Reject additional full-epoch traversals that bypass this seam. Verify matched scaling measurements for both one-module and many-module inputs and exact tested source identities before crediting the optimization.
- **Shared Enriched peephole catalog** (`pass/optimize.rs`) —
  `optimize_expr_in_mode` owns `project_after_tuple`,
  `compose_recovered_tail_access`, `let_bound_tuple_projection`,
  `inject_then_match`, `absurd_propagate`, `match_fusion`, `let_fuse`,
  `beta_reduce_unary_iife`, `beta_reduce_nullary_iife`,
  `dead_code_elimination`, `newtype_identity_elision`, and `constant_fold`.
  Package, per-module, and package-file entry points reach this same catalog;
  `cache/enriched.rs` uses the per-module/package-file entry points on its
  miss and disabled paths. Purity checks delegate to `pass/purity.rs`.
  Each rule preserves language behavior and is perf-only; discharge its
  firing/no-op coverage and recorded measured win at the truthful layer.
- **Shared callable-purity proof** (`pass/purity.rs`, `pass/structural_recovery.rs`, `pass/optimize.rs`, `cache/enriched.rs`) — inspect both conditional and either recovery plus let fusion/DCE for reuse of the same conservative proof. A declaration marker alone does not establish effect-free invocation: supplied values must be capability-free, lexical shadows must mask declaration facts, and unknown factories retain eager source order. Imported markers enter each consumer's enriched-cache key; declaration purity survives Prime, Enriched, and Routed without a new source-level type rule or package-wide effect solver.
  `pure_handler_factories_delay_into_both_dispatch_kinds` pins private recovery firing;
  `pure_declaration_cannot_hide_an_unknown_callback_effect` and
  `local_factory_shadow_keeps_both_dispatch_kinds_eager` pin its conservative boundaries.
  `declared_pure_calls_feed_dce_without_overriding_lexical_binders` and
  `shared_purity_distinguishes_closure_construction_from_invocation` pin optimizer firing and shadow/callable controls.
  `imported_purity_is_keyed_and_survives_every_enrichment_path` pins direct/disabled/miss/hit equivalence, imported-marker invalidation, and retained purity.
  The [dispatch evidence](../../../evidence/backend-performance.md#callable-purity-dispatch-recovery)
  records avoided work and bounded compiler cost; retain all-backend runtime
  controls for the semantic boundary.
- **Imported newtype-variance memo** (`pass/typecheck_core/variance.rs`) — strict-positivity checking derives an exact imported provider module's newtype variance table once per request and reuses the ready table at every branching occurrence. Variance composition and the in-progress fail-closed guard are correctness-primary; retaining and reusing a ready provider table is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#imported-newtype-variance-memo).

- **User-elaborator artifact/implementation pairing and analysis reuse** (`pass/typecheck_full.rs`, `pass/typecheck_core/{intern,memo}.rs`) — retains each validated evaluator artifact together with its exact staged implementation and singleflights it by provider module, elaborator name, and ordered capture imports for one immutable analysis. Eager result inference and deferred consumer batches therefore reuse the same exact artifact without carrying it into another analysis. Structural types may be interned across package versions, while all content-dependent `MemoCtx` state belongs to an opaque package-version revision selected by the LSP's package fingerprint and the artifact table belongs to a fresh scope bound to the current package analysis. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#user-elaborator-artifactimplementation-pairing-and-analysis-reuse).   Also check artifact-miss selection before copying: both `filtered_eval_package`
  and the package-less branch of `staged_user_elaborator_artifact` must use
  `filtered_eval_module` without changing source cutoffs, exact function
  identities, non-function items, metadata, imports or later validation.
  `eval_module_copy_preserves_exact_module_and_clones_only_retained_items`
  compares complete modules across cutoffs and module identities and counts
  the actual clone stream. Restoring cloning before filtering must fail its
  work assertion even while module equality passes.
  `eval_module_copy_cost` supplies the bounded same-binary legacy/candidate
  comparison; its recorded ordinary-control and body-size measurements are in
  [the artifact-copy evidence](../../../evidence/compiler-performance.md#elaborator-artifact-selection-before-copying).
  These establish artifact-copy savings, not per-call body sharing,
  whole-check speedups or a peak-memory win.
- **Ordered cancellable focused-module force/lower fan-out** (`cmd/check.rs`) — maps the focused request's already-selected conservative same-package dependency closure through the repository parallelism shim. Indexed collection retains sorted module order; errors are selected before target-package merge, and a cancelled or partial batch is never published; only a complete green batch is merged. This is perf-only; the closure itself and its laziness boundary are unchanged. [Recorded comparison and controls](../../../evidence/compiler-performance.md#ordered-cancellable-focused-module-forcelower-fan-out).

- **Run-scoped cross-package typed-module reuse** (`cache/typed.rs`, `cmd/check.rs`, `ci/run-tests.sh`) — stores typed Prime modules under a content-addressed key made from the compiler/cache identity, pipeline, declared module path, exact source, and sorted reachable written-import and elaborator dependency fingerprints; operator and fold callable targets reach providers through those written imports. The package name remains only a diagnostic label: each package's file, bridge contract, build configuration, visibility edges, and assembled Prime artifact are independently rebuilt and validated before or after lookup at their owning boundaries. Ordinary harness-owned paths give already-cache-enabled packages one invocation-scoped shared typed root. A custom `run.sh` remains isolated under its per-case, per-implementation scratch directory unless the regular golden orchestrator activates a fixed harness-owned cohort ID whose canonical exact script paths and Git object IDs are validated and frozen before dispatch; only authenticated cohort members inherit the cohort's separate invocation-scoped root. Relative debug-root overrides are ignored. Package-local clears and `cache ();` / `--no-cache` remain authoritative. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#run-scoped-cross-package-typed-module-reuse).

- **Disabled semantic-cache zero-work gates** (`cache/{artifact,emit}.rs`, `cmd/{build,check}.rs`, `backends/js/emit.rs`) — a globally disabled cache and a package-local `cache ();` keep semantic validation and uncached emission but bypass package-check whole-source hashing, artifact fingerprint rendering, and JS / Kio-prime emit-key context rendering and serialization. Active package-check caches still warm during `kio build`; active emit and artifact caches retain their original key paths. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#disabled-semantic-cache-zero-work-gates).

- **Equiv-cache shared streaming key prelude** (`cache/equiv.rs`, constructed by `cmd/test.rs`) — hashes the body-free package structure and other package-global evaluator inputs once before the parallel per-`equiv` fan-out, while each key hashes only its owner-aware reachable function-body closure; the sanitized representation streams into a fixed-size digest without materializing a JSON-shaped copy. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#equiv-cache-shared-streaming-key-prelude).

- **Linear recursive-signature context partition** (`pass/resolve.rs`, `sig/validate.rs`) — exact context/SCC congruence is correctness-primary. Hash-indexed binder shadowing, the signature-only duplicate-tolerant edge view, comparison-free SCC canonicalization, contiguous module runs, context hashes, the matched-member bitmap, and the ordered retained/component merge are its expected-linear realization. [Recorded comparison and controls](../../../evidence/compiler-performance.md#linear-recursive-signature-context-partition). During audit, reject a binder-vector membership scan, sorted/deduplicated diagnostic-edge construction on the signature-only path, comparison sorting inside `strongly_connected_components`, per-context graph analysis, or a difference-scan counter detached from the production comparison.

- **Direct recursive-component classification** (`pass/resolve.rs`, `pass/label_elab/mod.rs`) — classifies each existing SCC by its size or singleton self-edge, without retaining an index or phase state. [Recorded comparison and controls](../../../evidence/compiler-performance.md#direct-recursive-component-classification). During audit, inspect all four callers and require each separately restored list scan to fail its work guard; reject counters detached from actual queries or singleton-edge traversal.

- **Ordered recursive-group recovery correspondence** (`pass/route.rs`) — pairs source and phase-rebranded recursive-type members through the converter's one-for-one order, checks cardinality, variant, name, and binder preservation as internal phase contracts, and unfolds each newtype payload under only its own declaration binders. Direct pairing is perf-only; exact source ownership and payload unfolding are correctness-primary. [Recorded comparison and controls](../../../evidence/compiler-performance.md#ordered-recursive-group-recovery-correspondence).

- **Exact nominal-scope/provider acceleration and persistent alias contexts** (`pass/resolve.rs`, `pass/typecheck_core/{aliases,persistent_exact}.rs`) — indexes each module's written type-shaped selective and qualified type-import edges once, shares present indexes across phase-preserving clones, caches exact provider/owner results for one call, extends alias lexical/alpha/frontier state through structurally shared exact nodes, memoizes the free-rigid-name summary of each exact selected virtual view, and threads one incremental alpha allocator across nested alias owners. Resolution and capture hygiene are correctness-primary; the optional index, lazy provider caches, and persistent representation are perf-only realizations of that authority. [Recorded comparison and controls](../../../evidence/compiler-performance.md#exact-nominal-scopeprovider-acceleration-and-persistent-alias-contexts).

- **Prepared boundary-facade indexes and Python annotation batching**
  (`backends/boundary_facade.rs`, `backends/python/stub.rs`) — build live
  nominal/callable indexes once per facade transaction. Protocol localization
  parses/walks each annotation once for all captured roots and simultaneously
  rewrites exact tokens, preserving mixed variance, overlapping identifiers and
  ordinal holes. TypedDict parameter selection indexes current candidates and
  scans fields once per instantiation; cached definitions never supply stale
  concrete application arguments. Preserve zero-work empty-candidate guards.
  [Operation counts, parity controls and natural-wide validation](../../../evidence/backend-performance.md#prepared-boundary-facade-indexes-and-python-annotation-batching)
  separate unchanged output growth from causal analysis-work savings.

- **Backend-local prepared owner, reachability, and pending-work indexes**
  (`backends/rust/emit.rs`, `backends/java/{skin,emit}.rs`,
  `backends/swift/{skin,emit}.rs`, `backends/ts/emit.rs`) — check exact
  indexed newtype-payload ownership in Rust nested-forall naming and Java
  reverse lookup, Swift exact reached-nominal selection, and deterministic
  TypeScript pending-set consumption. Require parity with the selected owner,
  binding set or order and causal guards at production selection seams, not
  comparator-only counts. The
  [bounded operation evidence](../../../evidence/backend-performance.md#backend-local-prepared-owner-reachability-and-pending-work-indexes)
  neither measures wall time nor claims ordered collections are O(1).
- **Shared annotation/header planning and persistent lexical view** (`pass/typecheck_core/{annotation_plan,persistent_exact,kind_scheme,apply}.rs`, `prime/typer.rs`) — the one pre-mutation plan and its exact source-occurrence relation are correctness-primary. Borrowing a stack-local lexical slice, promoting one persistent exact-name prefix for movable plans, sharing that prefix among siblings, and suppressing transport state on canonical non-alias inputs are perf-only realizations. [Recorded comparison and controls](../../../evidence/compiler-performance.md#shared-annotationheader-planning-and-persistent-lexical-view).

- **Empty-hole completed-argument equivalence** (`pass/typecheck_core/apply.rs`, exercised through `prime/typer.rs`) — completed arguments still require final equality. When the application-hole parameter and substitution sets are both empty, a successful ordinary identity-aware equivalence check returns without constructing, cloning, normalizing, or reconciling the generic hole-solver transaction. A mismatch falls through to the unchanged structural unifier so its leaf diagnostic and expected-source span remain stable; every nonempty state also retains that solver. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#empty-hole-completed-argument-equivalence).

- **Empty-substitution semantic-call handle reuse** (`pass/typecheck_core/apply.rs`) — selection reuses an immutable interned solver, written-diagnostic, or result handle only when its substitution map is empty and the computed identity-canonical bit exactly equals the handle's existing bit. Nonempty substitutions and provenance downgrades retain the former substitution and reconstruction path. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#empty-substitution-semantic-call-handle-reuse).

- **Goal-free retained-publication validation proof** (`pass/typecheck_core/goals.rs`, transported by `pass/typecheck_full/publication.rs`) — the first close and every goal-bearing retained close still perform owner/scope and goal-usability checks, canonicalization, zonking, function-scheme validation, kind checking, and publication-escape validation. Only a final identity-canonical, goal-free result mints the private move-only proof. Its intrinsic key is the owned exact `ScopedType` and rigid scope (which carry store, package-analysis context, module, and binder identities), the sealed publication root kind, and the retained output's exact next destination. A later hop still checks the current store/context, live delta and owner lifecycle, direct-parent/root destination, source-scope extension over the current owner, extension over the new destination, source order, prepared revision, and atomic commit. It skips the already-completed sealed-value walks for scoped goal-chain and goal usability, canonicalization, zonking, function-scheme shape, kind, and publication escape. This is not a reusable cache: before the proof exists, every goal/policy/delta revision stays authoritative through the old path; after no goals remain, later writes cannot change the owned type, and the enclosing publication plan consumes the proof once. The proof is Lowered-only transient state and grants no serialized, cached, Prime, reflection, or producer-provenance authority. [Recorded comparison and controls](../../../evidence/compiler-performance.md#goal-free-retained-publication-validation-proof).

- **Deferred-tail classifier arena and scoped aliases** (`pass/typecheck_core/apply/fills.rs`) — candidate topology and lexical shadowing are correctness-primary. Copyable symbol handles into an invocation-local pair arena, per-role pair marks, and insert/restore alias bindings are perf-only realizations that avoid expanding shared pair topology or cloning the live alias map. [Recorded comparison and controls](../../../evidence/compiler-performance.md#deferred-tail-classifier-arena-and-scoped-aliases).

- **`SharedRoutedPackage`** (`cmd/build.rs`) — computes the route + capability passes once and shares the result across a multi-target build via `OnceLock`. This is **correctness-primary, not perf-only**: the up-front `routed.get()` force (gated on `>1` selected target) averts a rayon-pool deadlock that a lazy `get_or_init` triggers when parked inside the parallel per-target dispatch. The compute-once sharing is a behavior-preserving side benefit of that force. Discharge: the force is load-bearing (removing it deadlocks multi-target builds — a hard failure, not a silent regression), so no separate perf measurement is required; a rot to per-target recompute would still be correct, only slower.
- **Runner compile profiles** (`ci/infra/kio-test-runner-rs/src/shared/opt_profile.rs`) — the per-backend test runners expose selectable `unoptimized` / `default` / `optimized` optimization profiles (`--profile` / `KIO_TEST_RUNNER_PROFILE`). `default` preserves each backend's current cheap level (rustc `opt-level=1`, but ghc `-O0` / swiftc `-Onone` — their mild tiers cost real compile time, unlike rust's near-free `opt-level=1`), and `optimized` is the on-demand higher tier (rustc `opt-level=2` / ghc `-O2` / swiftc `-O`). These **change observable output** — a different opt level is a different compiled artifact, and the profile is folded into the profile-keyed artifact cache — so they are **not** perf-only and need no separate measurement. [Recorded comparison and controls](../../../evidence/compiler-performance.md#runner-compile-profiles). No perf-only complexity remains from this lever; do not re-add it on an assumed win.
- **Native resource engine and adaptive compiler feedback** (`ci/infra/kio-ci-scheduler-rs/`, reached through `ci/schedule.sh`) — one Git-common Rust state machine owns work, optional capacity-one Cargo, and compiler claims; the shell remains a facade. Immutable leases, transition markers, inherited Unix descriptors, Windows suspended spawn/Job supervision, isolated readiness, fixed-capacity minima and FIFO are correctness/portability boundaries. Best-effort feedback is a performance mechanism, not a universal OOM guarantee: one bounded transient record belongs to the existing lock; only the valid FIFO head advances it; fixed-only activity bypasses it; idle and failed probes retain throttling; corrupt, stale, incomparable or unwritable state falls back conservatively. Inspect the native adapters' units, available CPU scope, memory estimate, failure paths and bounded cost. Clients honor every live adaptive CPU ceiling and explicit fixed minimum. The injected policy/store tests in § 7 require growth above two, no growth without demand/headroom, pressure backoff without holder cancellation, cadence shared across heads, reset, fixed-only bypass, and current-client CPU and fixed-capacity minima. Artificially restricting the feedback target at the actual admission seam must fail the unchanged growth/admission guard. Require recorded workload evidence with exact source/tool/seed/cache identities before crediting a throughput win; classify matching, contention and uncertainty explicitly and limit causal claims accordingly. The ordinary no-override path must actually exercise feedback. Record probe/fallback behavior and aggregate resource pressure, not only largest-child RSS. Platform-independent pure tests do not establish native macOS/Windows probe or kernel behavior. The v3 trace stays outside the state lock, uses `adaptive-cap`, and emits null rather than invented targets when feedback was not consulted. Existing trace, lease, readiness, native self-test and process-tree controls remain authoritative. [Recorded comparison and controls](../../../evidence/compiler-performance.md#native-resource-engine-and-adaptive-compiler-feedback).

- **Standalone Cargo compiler-only admission** (`ci/cargo.sh`) — a top-level Cargo command takes the optional scheduler-native capacity-one `cargo` resource and one compiler permit without also taking `work`. Cargo invoked by an already-scheduled corpus worker retains that inherited work lease. This prevents a queue of independent top-level Cargo commands from blocking admission of corpus work while preserving `work -> cargo -> compiler` order. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#standalone-cargo-compiler-only-admission).

- **Persistent corpus-tool Cargo targets** (`ci/checks/orchestrators/lib/common.sh`) — exactly four corpus orchestrators reuse dedicated worktree-local Cargo targets for the identical Prime-only compiler and grammar-verifier builds. Every normal caller still invokes Cargo, whose fingerprints remain the sole staleness authority; one explicit generic cargo lease spans build and private copy, and `ci/cargo.sh` adds compiler without giving standalone helpers a work claim. The scheduler bypass retains invocation-private targets. This is perf-only. `test_stable_corpus_tool_target` pins one actual fake construction from four normal Cargo invocations, exact stable-target identity, scheduled `work -> cargo -> compiler` and standalone `cargo -> compiler` order, copy under cargo admission into four private space-bearing paths, and the bypass no-op boundary. Replacing the stable normal target with invocation-private targets or removing explicit cargo admission makes the focused self-test fail.
  [Recorded comparison and controls](../../../evidence/compiler-performance.md#persistent-corpus-tool-cargo-targets).

- **Bounded-pass `run-tests.sh` worklist construction and shared package
  preflight** (`ci/run-tests.sh`) — one corpus walk inventories case markers,
  package paths, and generated-output markers; bounded joins classify each
  selected case's package shape and root targets once, form one canonical
  case/implementation applicability relation, and derive implementation
  sampling and `(case, binary)` units without per-pair filesystem probes. Exact
  standard-runner package-shape validation is correctness-primary; the shared-
  inventory and bounded-join realization is perf-only. It preserves target and
  compile-only eligibility, configured implementation order, seeded case
  sampling, implementation candidate order, filters, and unit grouping.
  [Recorded comparison and controls](../../../evidence/compiler-performance.md#bounded-pass-run-testssh-worklist-construction-and-shared-package-preflight).

- **Exact-name selector batching** (`ci/run-tests.sh`) — conservatively classifies a complete positional-filter set as tightly anchored safe case names, matches that set with one `grep -E -f` pass, and otherwise retains the legacy per-case/per-selector matcher unchanged. Exact original case paths, discovery order, exclusions, and Prime/dynamic marker gates are preserved; classifier, matcher, staging, or remapping anomalies fail closed to the legacy path. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#exact-name-selector-batching).

- **Golden Kio' roundtrip multi-target batching** (`ci/checks/per-case/kio-prime-roundtrip.sh`) — combines the producer compiler's direct-target and Kio' builds for expected-success roundtrip rows. When the combined transaction fails, the production check replays the two legacy builds fail-closed, preserving target-inapplicability skips, failure attribution, and byte-for-byte output comparison. This is perf-only and preserves golden behavior and output. [Recorded comparison and controls](../../../evidence/compiler-performance.md#golden-kio-roundtrip-multi-target-batching).

- **Streaming and reusable dynamic-loader declaration scopes** (`test-data/poc/dyn_load_prime/workdir/loader.kio`) — resolution streams the unprocessed suffix and reversed processed prefix rather than reconstructing both from a numeric position. It rebuilds the complete declaration scope for the first declaration of a module, then advances and reuses that exact scope for consecutive declarations in the same module. An owner or cutoff discontinuity, or a source-local host function becoming visible between the two declarations, falls back to the complete rebuild. Scope contents, declaration/error order, source-order visibility, and the evaluator's de Bruijn chain are unchanged. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#streaming-and-reusable-dynamic-loader-declaration-scopes).

- **Exact public bridge scope for scripted dynamic-load hosts** (`test-data/goldens/00_success/exec_dyn_load_*`) — each custom host builds the complete copied loader/interpreter package but exposes only the root `testapi` module and the seven submodules used by its fixed runner protocol. Internal implementation modules remain ordinary imports and continue through typechecking and emission; runners that invoke `kio test` retain their package `equiv` checks. Only unused generated host facades disappear. This is perf-only. [Recorded comparison and controls](../../../evidence/compiler-performance.md#exact-public-bridge-scope-for-scripted-dynamic-load-hosts).

- **Phase-local dynamic-Prime driver preparation**
  (`ci/checks/orchestrators/golden-tests.sh`) — prepare the selected dynamic
  driver's existing publication immediately before dynamic execution, outside
  the failure-catching `run_tests` function. [Recorded comparison and controls](../../../evidence/compiler-performance.md#phase-local-dynamic-prime-driver-preparation).

- **Case-marker shell directory extraction inside shared discovery**
  (`ci/run-tests.sh`) — the combined corpus `find` prints each `expected.exit`
  marker alongside package-discovery inputs, and POSIX shell suffix removal
  derives its case directory without spawning one `dirname` process per marker.
  Root and repeated-slash paths retain `dirname`-equivalent normalization;
  selection, applicability, implementation sampling, unit grouping, and report
  order are unchanged. This is perf-only and distinct from the package-shape
  indexing performed by the surrounding shared inventory.
  `run-tests-predispatch-selftest.sh` requires that combined walk to use `-print`
  without `-exec`, pins the existing two-`find` bound, and exercises a
  mutation-live repeated-slash path control.
  [Recorded comparison and controls](../../../evidence/compiler-performance.md#case-marker-shell-directory-extraction-inside-shared-discovery).

- **Batched Prime-marker source discovery** (`ci/checks/per-case/prime-marker.sh`)
  — inventories tracked case paths once and classifies candidate headers in one
  ordered awk pass, without changing the verifier loop or marker policy. Git
  C-quoted paths use exact literal membership queries instead of a pathname
  decoder; traversal, index and read failures stop before verification or marker
  updates. This is perf-only for valid discovery inputs. The private
  `prime-marker-selftest.sh` pins bounded Git/header work, literal and symlink
  paths, generated drafts, caller locale, Unicode-normalizing Git membership,
  relative temporary paths, ordering, early verifier rejection, marker updates
  and discovery failures. Restoring the per-file loop makes its unchanged
  bounded-work assertion fail. [Recorded comparison and controls](../../../evidence/compiler-performance.md#batched-prime-marker-source-discovery).

- **Package-only optimization of CI compiler snapshots**
  (`ci/checks/orchestrators/lib/common.sh`) — the `kio-lsp-cli` and `kio-prime`
  tool keys compile only `kio-lang` at dev opt-level 2, with debug assertions
  and overflow checks enabled. Dependencies, runners, verifiers and ordinary
  developer/test/Clippy/release builds retain their existing profiles. Both
  scheduled and bypass routes use the same three overrides; persistent targets
  and Cargo fingerprints retain their existing ownership. This is perf-only.
  `schedule-entry-selftest.sh` checks both compiler keys on both routes and
  the verifier no-op control. Disabling the profile branch makes its unchanged
  exact-argv assertion fail; restoring the branch passes.
  The opt2 expectation also fails against the opt1 helper and passes with opt2.
  [Recorded comparison and controls](../../../evidence/compiler-performance.md#package-only-optimization-of-ci-compiler-snapshots).

For each retained performance-only realization under review, discharge both:

- **Firing coverage?** Use the narrowest truthful evidence. Observable cross-implementation language/CLI/runtime behavior is golden-pinned through the fixed runner; a golden case never reads, copies, greps, patches, imports, or native-compiles generated host-backend files. A durable generated-file proxy retained with the recorded measurement is an `ARTIFACT_SHAPE` emission, and it pins only that portable measured/spec fact. Exact private/no-filesystem firing belongs in unit or mutation coverage (`audit-reports`). Incidental private artifact assertions are deleted rather than promoted to corpus contracts.
- **Measured win?** It landed (or is retained) with a recorded measurement. Link the subject record in `evidence/`, including the comparison, workload, relevant versions/settings, result and limitations; an existing measurement can be reused while those assumptions still apply.

Kio' is the specified backend-neutral phase-artifact exception: a golden may
read or assemble it when the phase artifact, round trip, verifier/evaluator, or
dynamic-load boundary is the subject, and a generic harness-owned phase check
may do likewise. That exception does not authorize direct host-backend output
inspection. Emissions supplement runtime coverage and never satisfy a backend-
completeness runtime cell.

Findings:

- **Unjustified perf complexity** — retained perf-only complexity lacks causal firing coverage or a recorded measured win; supply the missing evidence or simplify/remove the machinery without weakening required behavior.
- **Unregistered perf-only optimization** — a substantial mechanism absent from the owning inventory; update that inventory and its applicable check and evidence, not a second catalog here.

## 9. Backend Emission And Host-Compiler Scaling

For each newly added or materially revised backend path, inspect both the
compiler and the generated artifact:

- preparation and rendering are bounded by the package/site/type inventory and
  do not repeat the same semantic transaction for sibling artifacts or each
  selected target without a correctness reason;
- wide products, sums, callable stages, and nested conversions do not expand
  through Cartesian matches, repeated right-spine projections, recursively
  duplicated expressions, or unbounded single lines;
- deep finite `rec(loop)` execution does not consume host stack proportional
  to iteration; specifically, a tail `Continue` must not repeatedly wrap an
  exact carried function value already in the destination slot representation
  when no semantic conversion remains. Require a documented fixed-depth
  structured-state runtime witness GREEN on every backend with its exact
  program, iteration count, and expected result. For each semantically distinct
  adaptation owner or realization, that same input must reach execution and
  fail from wrapper/host-frame growth under the wrapper-growing behavior, then
  pass after restoration; pair it with a causal guard at the owning adaptation
  layer. One shared-layer RED/restored-GREEN pair covers all consumers of that
  layer; each backend-local owner requires its own pair. Its controls retain
  same-ABI real conversion, different-ABI adaptation, and genuine pending
  continuation work;
- a natural wide package records generated bytes, maximum line/nesting or an
  equivalent portable structural metric, and the real host compiler's focused
  wall time and peak RSS beside a narrow control;
- every performance-only bounded realization has a causal firing/no-op guard
  plus a measured win, while a rejected idea leaves no production machinery.

Use admission, change-time and periodic review as described in
[Backend quality and implementation cost](../../topics/emit.md#backend-quality-and-implementation-cost).
Beyond regressions, select bounded improvement opportunities from changed
assumptions, new backend lessons, weak evidence or previously unexamined owners.
Include simplification and shared ownership, not just faster execution. State
the selection and limits so later audits can vary their attention; findings
do not authorize unrelated implementation. A family label does not exclude a
semantic peer, and this requires neither all-pairs comparisons nor equal speed.

This is a proportionality audit, not a fixed speed contest. Flag superlinear or
duplicated work, missing wide evidence, and resource mitigation presented as a
root-cause fix. Do not require a broad benchmark for an edit that cannot affect
backend preparation, emitted shape, or native compilation.

Audit generated execution as well as compilation using
[`add-backend`'s comparative runtime check](../add-backend/SKILL.md#comparative-runtime-check).
A full backend review covers every in-scope backend; a narrow change targets
affected paths and useful peers. Reuse comparable retained evidence, obtaining
bounded measurements where it is missing or no longer represents the path under
review. The shared procedure prefers representative castles, separates startup
and execution, and accounts for actual profiles and runtime conditions. Flag
missing comparative evidence and unexplained large gaps, not violations of a
presumed language ranking. Do not turn this review into automatic whole-corpus
timing runs or ordinary wall-time test assertions.

For conversion-heavy paths, apply
[conversion placement and cost](../../topics/emit.md#conversion-placement-and-cost).
Separate plan-cache misses from per-value copying, conversion and callable
dispatch; frequent events are not additive wall-time fractions. Compare against
a representative earlier implementation as well as the immediate candidate.
Check the shared host-value responsibility: general runtime type validation is
not part of the generated host interface. Distinguish that from conversions
needed by valid programs and authorized opaque-payload access.
Treat conversion-disabled counterfactuals as attribution only, and require
semantic evidence for each production elimination. A correctness repair explains
why an obligation exists, not why every internal call must pay for it.

Include representative elaborator-heavy programs, preferring castles for
realistic feature combinations and using POCs or focused cases for patterns
those castles do not cover. Trace expansion into Kio' through
shared optimization and host emission, looking for avoidable closures, adapters,
intermediate values, repeated traversals and duplicated work. Measure retained
costs and identify the phase that owns them. Existing elaborators supply useful
workloads, not compiler privileges: recommendations must apply to general
resolved/typed structures, including equivalent user-written elaborators, never
special-case declaration names or package identities. Not every elaborator
needs a new optimization; findings require concrete unnecessary work or missing
evidence, not merely an unfamiliar emitted shape.

The Python typed-stub package is one registered correctness-primary scaling
mechanism. Check its bound and causal guards explicitly:

```sh
grep -nE 'PYTHON_STUB_DECLARATIONS_PER_SHARD|shard_python_stub|lower_package_to_stub_package' \
  kio-rs/src/backends/python/stub.rs kio-rs/src/cmd/build.rs
grep -nE 'stub_sharding_bounds_modules|missing generated declaration shard|missing shard re-export|800|required mypy|mypy 2.4.0' \
  kio-rs/src/backends/python/stub.rs \
  ci/checks/repo-lint/pyright-strict-selftest.sh
```

Confirm that the public entry point re-exports every deterministic private
shard, each declaration has one owner, cross-shard references retain that
owner's exact identity, and the 256-declaration ceiling has not been weakened.
The self-test must execute a greater-than-256 nominal/TypeVar cycle through
Pyright, run the required pinned mypy 2.4.0 positive and negative replay, and
prove that large single Protocol and TypedDict member bodies do not hide a
second Pyright complexity
ceiling. Recheck the portable natural-wide and narrow measurements in
[the typed-stub evidence](../../../evidence/compiler-performance.md#bounded-python-typed-stub-packages) when the layout, bound, or checker version
changes.

## How To Report

Group findings by severity:

1. **Interactive regression** — LSP main-loop blocking, lost cancellation/debounce/focused analysis, unbounded focused-shard or watermark retention, stale snapshot overwrite, or newer-shard eviction.
2. **Warm-build/check regression** — cache bypass, under-keyed cache, lost package-check skip, typed-module/enriched/emit/artifact cache breakage.
3. **Parallelism regression** — lost independent fan-out or non-deterministic parallel output.
4. **Laziness regression** — broad eager body parsing/forcing where the workflow only needs headers or one module.
5. **Inventory drift** — unregistered optimization, stale row, or audit gap.
6. **Observability gap** — missing or misleading cache/timing probes.
7. **Backend scaling regression** — repeated facade preparation, superlinear
   emission, pathological generated shape, disproportionate host-compiler
   wall/RSS on the natural wide witness, or recursive host-stack growth on the
   fixed-depth structured-state witness; also unexplained generated-execution
   gaps against comparable peer workloads.
8. **Test gap** — no focused guard for a performance-architecture change, or
   missing per-backend runtime, wrapper-growing RED/restored GREEN, or semantic
   boundary controls for recursive stack safety.
9. **Evidence misplacement** — a golden inspects generated host output, an
   `ARTIFACT_SHAPE` pins an incidental/private detail without a durable spec or
   recorded measurement, or emission evidence is credited as runtime coverage.

For each finding, cite `file:line`, explain the workflow impacted (`kio build`, warm `kio check`, LSP diagnostics, typed LSP request, docs/tests), and state the smallest corrective direction. Distinguish true findings from deliberate sequential code; the greps over-match by design.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). Fix the architecture or add missing regression guards; do not paper over a real performance regression with a comment.
