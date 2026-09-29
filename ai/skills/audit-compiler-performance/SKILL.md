---
name: audit-compiler-performance
description: Audit the compiler and compiler-producing CI performance architecture — native resource scheduling, parallel fan-out, lazy parsing, semantic on-disk caches, LSP responsiveness, backend emission and recursive host-stack scaling, and timing probes — for regressions as features land
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

Read [`ai/topics/implementation.md` § Compiler performance architecture](../../topics/implementation.md#compiler-performance-architecture). Treat its table as the registry of concrete performance mechanisms the audit must preserve.

Check that the registry names every substantial compiler-performance mechanism currently in the codebase:

```sh
git grep -nE 'cache|Cache|memo|Memo|interner|Interner|lazy|Lazy|debounce|focused|snapshot|CancellationToken|maybe_par_iter|par_iter|frontend-timing|lsp-timing|KIO_DEBUG_(TIMING|.*TIMING|.*CACHE|CI_SCHEDULER_TRACE|SAMPLE_IMPL_SEED|TEST_RUNNER_COMPILER_OBSERVER)|compiler_observer|available_parallelism|AdmissionEvaluation|ClaimStore|readiness_hook|JobObject' -- kio-rs/src ci/infra/kio-ci-scheduler-rs/src ci/infra/kio-test-runner-rs/src ci/all.sh ci/run-tests.sh
git grep -nE 'build_corpus_tool_binary|kio-corpus-tools' -- \
  ci/checks/orchestrators ai/topics/implementation.md
```

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

Per AGENTS.md § Universal rules ("Optimizations are justified, not assumed"), a change defended as a performance optimization that does *not* change observable output must carry a recorded measurement of its win and must not rot into a no-op. No audit can mechanically verify a measurement exists, so — like `audit-spec-drift` § 7's per-carve-out discharge — this section **lists** the codebase's performance-only optimizations and forces a per-item discharge.

Enumerate the performance-only optimizations (behavior-preserving complexity whose justification is speed). Current registry:

- **Package-scoped signature identity index** (`pass/typecheck_core/modules.rs`, `pass/resolve.rs`) — a complete package-signature check shares one existing exact package-owned scope across its module checks. Every module still passes the original scope pointer assertions and signature checks with the same execution policy; individual-module checks and later package-check invocations create fresh scopes. The lazy identity-alias index cannot outlive or cross the invocation's exact package authority. `signature_package_builds_one_identity_alias_index_across_modules` observes actual index construction and visited items, including independent `T = .` aliases that have no identity-alias edge: 64 and 128 modules require one build and respectively 64 and 128 item visits. Restore fresh scopes per module and require this unchanged test to fail; an alias-edge-only counter misses the wasted work. `signature_index_reuse_stays_within_one_package_check` pins fresh package invocations, fresh individual-module checks, and the empty-package no-op. `signature_package_scope_matches_fresh_module_checks` compares accepted and rejected complete results with the former independently scoped module loop, including distinct imported identities, alias/newtype dependencies, higher-kinded arguments and invalid type applications, including same-spelled aliases with distinct provider arities. Require matched final-byte measurements with a bounded bridge configuration as well as exact signature output equality; retaining an independently growing bridge-selector axis does not establish declaration-only scaling.
- **Indexed fresh-signature origin slices** (`sig/validate.rs`, `sig/replay.rs`) — origin imports and final full-epoch semantic validation retain their existing authority. A candidate-module index and name-only dependency slices avoid rebuilding all declarations separately for origin resolution and identity qualification. `fresh_canonicalization_work_is_linear_for_independent_declarations` observes the actual `build_package_from_modules` seam during canonicalization: 64 and 128 independent aliases require respectively 64 and 128 constructed module items and packages. Require separate causal failures when the full name-only resolution view or the full qualification package is restored; a caller-side slice/fallback tally cannot detect an extra package construction. Final epoch construction collects each module's first present compile-time import in its existing declaration pass, before recursive-context deduplication. `fresh_epoch_construction_work_is_linear_across_modules` drives replay over 64 and 128 modules and counts actual declaration-iterator visits in `epoch_modules`: 128 and 256 visits across pre-removal and post-removal validation. Restore the old per-module scan through that observed iterator and require its independent failure. Reject additional full-epoch traversals that bypass this seam. `epoch_modules_preserve_first_comptime_import_and_origin_override` pins first-present order, origin override including an empty list, recursive-context deduplication, and empty-module behavior. The import fixtures pin selected identities, while `fresh_origin_slice_matches_full_epoch_resolution_and_qualification` and `fresh_origin_slice_matches_full_epoch_recursive_context` compare accepted canonical declarations and rejected diagnostics with complete origin/epoch construction. Independent bounded-size declarations with bounded imports have bounded per-origin construction and one traversal per final epoch construction even when spread across modules; ordered indexes retain logarithmic factors, and these guards make no all-histories linearity claim. Verify matched scaling measurements for both one-module and many-module inputs and exact tested source identities before crediting the optimization.
- **`pass/optimize.rs`** arms — match fusion, project-after-tuple, let-immediate-use, newtype-identity elision, constant folding, DCE, CSE, … Each preserves behavior, so each is perf-only.
- **Imported newtype-variance memo** (`pass/typecheck_core/variance.rs`) — strict-positivity checking derives an exact imported provider module's newtype variance table once per request and reuses the ready table at every branching occurrence. Variance composition and the in-progress fail-closed guard are correctness-primary; retaining and reusing a ready provider table is perf-only. Discharge (deterministic provider-derivation counts, not wall time): `imported_variance_memo_avoids_branching_provider_rederivations` checks the same accepted nested-contravariant payload with and without ready-result reuse. Across a five-provider chain with two references at every link, the memoized path derives `p0` through `p4` once each (5 derivations total); the test-only ready-reuse-disabled comparator derives them 512/128/32/8/2 times respectively (682 total). Removing the ready hit makes the exact count assertion fail while both arms preserve the positivity result. This evidence makes no wall-time, CPU, RSS, allocation-rate, or end-to-end performance claim.
- **User-elaborator artifact/implementation pairing and analysis reuse** (`pass/typecheck_full.rs`, `pass/typecheck_core/{intern,memo}.rs`) — retains each validated evaluator artifact together with its exact staged implementation and singleflights it by provider module, elaborator name, and ordered capture imports for one immutable analysis. Eager result inference and deferred consumer batches therefore reuse the same exact artifact without carrying it into another analysis. Structural types may be interned across package versions, while all content-dependent `MemoCtx` state belongs to an opaque package-version revision selected by the LSP's package fingerprint and the artifact table belongs to a fresh scope bound to the current package analysis. This is perf-only. Discharge: on a cold `kio test --no-cache` run of the package fixture used by `exec_dyn_load_exact_descriptors`, the blocked per-module-batch implementation took 192.88 seconds wall / 796.37 seconds CPU / 1,975,008 KiB peak RSS; package-check reuse took 53.66 seconds wall / 107.35 seconds CPU / 1,961,900 KiB peak RSS (72.2% less wall time and 86.5% less CPU, with effectively unchanged memory). The latter run exercised four retained artifact identities (1,879 hits / 4 misses / 4 first writes), not a single-key microcase. On the fixed single-thread elaborator proxy, wall time fell from 12.51 to 4.53 seconds and CPU from 5.84 to 4.52 seconds. `user_elaborator_artifact_pairs_are_analysis_scoped_singleflight` pins exact pairing, capture separation, success-only caching, singleflight, and analysis lifetime; `reused_structural_interner_does_not_reuse_package_content` pins the raw-interner boundary; `package_versions_share_structural_types_but_not_content_memos_or_analysis_artifacts` pins all three ownership layers; `revision_store_reuses_only_exact_versions_and_binds_each_scope`, `lsp_user_elaborator_memo_survives_repeated_same_snapshot_analysis`, and `lsp_user_elaborator_memo_invalidates_after_provider_edit` pin revision replacement, analysis-owner binding, same-fingerprint warm reuse, and provider-edit invalidation. `lsp_missing_focus_fallback_reuses_worker_memos` pins the target-disappearance fallback to that same revision store: two identical fallback analyses record one miss and one hit rather than leaving the caller's store empty. `imported_consumer_batches_share_one_exact_artifact_per_package_check` and `eager_result_inference_and_deferred_check_share_one_exact_artifact` pin the two reuse paths; `staged_provider_helper_reconstructs_imported_surface_elaboration` pins ambient-free provider reconstruction.
- **Ordered cancellable focused-module force/lower fan-out** (`cmd/check.rs`) — maps the focused request's already-selected conservative same-package dependency closure through the repository parallelism shim. Indexed collection retains sorted module order; errors are selected before target-package merge, and a cancelled or partial batch is never published; only a complete green batch is merged. This is perf-only; the closure itself and its laziness boundary are unchanged. Discharge: exact `77274b4397c66d79152fad63e390128da0d99389` baseline and integrated-candidate release binaries drove fresh real `kio lsp` processes with eight Rayon threads in six counterbalanced samples per arm. On the natural `library_catalog_composite` castle focused at `testapi/main.kio`, median internal foreground analysis fell from 655.492 to 510.801 milliseconds (22.074%) and matched external request-to-analysis latency fell from 656.711 to 511.947 milliseconds (22.044%); every candidate sample was faster than every baseline sample. Median focused-completion CPU rose from 0.980 to 1.035 seconds (+5.612%), and sampled focused peak RSS rose from 189,578 to 193,478 KiB (+2.057%). All twelve analyses returned `outcome=ok` and byte-identical hover responses. Historical pre-rebase controls on exact `317aa9c91b541df4416c757b59665ba5818054fc` amplified the causal effect on a body-heavy 24-module package (3,011.491 to 1,933.115 milliseconds, 35.809%, at +2.559% sampled peak RSS), put the natural dict POC's 1.862% change within its sample spread, and bounded one-module no-material-fan-out overhead below 0.14 milliseconds in median external latency. Measurement processes were stopped immediately after the verified response so the independently debounced full analysis could not contaminate focused latency or RSS; this gives no broad-gate credit. `focused_force_collection_overlaps_work_and_preserves_input_order` is genuinely red with the serial iterator and pins both firing and ordered collection; `focused_force_collection_cancels_before_queued_work` runs through the same helper with a one-thread pool and through the no-`parallel` fallback; `focused_force_collection_keeps_first_error_in_input_order` and `lsp_focused_force_reports_the_same_first_module_error` pin deterministic private and end-to-end error selection.
- **Run-scoped cross-package typed-module reuse** (`cache/typed.rs`, `cmd/check.rs`, `ci/run-tests.sh`) — stores typed Prime modules under a content-addressed key made from the compiler/cache identity, pipeline, declared module path, exact source, and sorted reachable ordinary, elaborator, and direct operator/fold-callable dependency fingerprints. The package name remains only a diagnostic label: each package's file, bridge contract, build configuration, visibility edges, and assembled Prime artifact are independently rebuilt and validated before or after lookup at their owning boundaries. Ordinary harness-owned paths give already-cache-enabled packages one invocation-scoped shared typed root. A custom `run.sh` remains isolated under its per-case, per-implementation scratch directory unless the regular golden orchestrator activates a fixed harness-owned cohort ID whose canonical exact script paths and Git object IDs are validated and frozen before dispatch; only authenticated cohort members inherit the cohort's separate invocation-scoped root. Relative debug-root overrides are ignored. Package-local clears and `cache ();` / `--no-cache` remain authoritative. This is perf-only. Discharge: two fixed 16-file, 603,268-byte golden packages (`exec_match_single_polymorphic_scalar_call` and `exec_narrow_sum_checked_term_scope`) contain 14 typed modules with 13 byte-identical semantic inputs. Four fresh-root matched pairs in counterbalanced `ABBA BAAB` treatment order, with both `XY` and `YX` package order, compared package-local roots against one cross-package root. All four pairs were directional. The measured implementation includes cycle-complete iterative reachability, exact-key plus encoded-body integrity validation, and global/package-disabled zero-work paths. The median second-command wall reduction was 23.2178% (pair gains 30.9478%, 17.5926%, 28.4810%, and 17.9545%); median two-command wall reduction was 10.8282% (12.6397%, 10.7143%, 8.5169%, and 10.9420%); median summed user+system CPU reduction was 32.2340% (33.0511%, 31.7386%, 31.5562%, and 32.7295%). Peak RSS stayed within the package-local arm plus the larger of 5% or 65,536 KiB in every pair. All 16 commands exited zero and reproduced package-matched Kio-prime output manifests; no sample was discarded or replaced. The untimed causal preflight recorded 0 hits / 14 misses / 14 writes for both package-local builds and the shared-root first build, then 13 hits / 1 miss / 1 write for the second shared-root package, whose sole miss was its distinct `testapi/main`; its package-local clear did not touch the shared root. This measurement gives no broad-gate credit. A production-path `ABBA BAAB` A/B over two exact custom dynamic-load goldens measured four fresh control invocations against four authenticated-cohort invocations. Median wall fell from 92.840 to 70.680 seconds (23.869%); all four matched savings were positive at 23.27, 21.58, 22.03, and 21.67 seconds, with median 21.850 seconds. Median matched user+system CPU saving was 75.470 seconds; every treatment/control RSS ratio was at most 1.028. Control cache evidence was 0 hits / 101 misses / 101 writes and treatment was 47 / 54 / 54, including the five distinct guest modules built by the scripts. All eight commands exited zero with identical normalized output, unchanged source/binary identities, and OOM 0 to 0; this focused measurement gives no broad-gate credit. `semantic_module_identity_reuses_entries_across_package_names`, the key-input tests, concurrent/late-publication and corrupt-destination tests, and flat-entry GC test pin private identity, publication, and storage firing. `typed_modules_reuse_one_semantic_entry_across_package_names` pins package/config/bridge/nominal/visibility/open-world and ordinary dependency invalidation boundaries; `operator_and_fold_callable_paths_are_same_package_dependencies` plus `direct_operator_callable_edit_invalidates_an_importing_consumer` pin the four implicit callable slots and their transitive invalidation; `typed_cache_debug_root_shares_only_explicitly_enabled_package_entries` and `typed_cache_debug_root_ignores_relative_paths` pin debug-root policy; `typed_cache_dependency_invalidation` pins source/dependency key movement in both frontends; `run-tests-progress-selftest.sh` pins default custom isolation, authenticated fixed-cohort sharing, frozen script bytes, and cleanup; and `coverage-policy-selftest.sh` pins regular-golden-only cohort activation.
  `every_root_of_a_three_cycle_has_the_complete_reachable_set` and `parsed_mixed_cycle_moves_downstream_fingerprints_after_member_edit` pin cycle-complete iterative reachability and implicit-callable propagation. `globally_disabled_typed_cache_does_no_dependency_or_key_work`, `package_disabled_typed_cache_does_no_dependency_or_key_work`, and `enabled_typed_cache_prepares_dependencies_and_key` causally pin the disabled no-op and enabled firing paths. `exact_key_header_rejects_a_different_valid_module_body` pins encoded-body integrity independently of semantic-key header validation; the existing concurrent, corrupt-destination, and retry controls exercise the same validated read path.
- **Disabled semantic-cache zero-work gates** (`cache/{artifact,emit}.rs`, `cmd/{build,check}.rs`, `backends/js/emit.rs`) — a globally disabled cache and a package-local `cache ();` keep semantic validation and uncached emission but bypass package-check whole-source hashing, artifact fingerprint rendering, and JS / Kio-prime emit-key context rendering and serialization. Active package-check caches still warm during `kio build`; active emit and artifact caches retain their original key paths. This is perf-only. Discharge (deterministic operation counts, not wall time): `globally_disabled_package_check_cache_skips_source_hash_work` and `package_disabled_package_check_cache_skips_source_hash_work` record zero source hashes, while `enabled_package_check_cache_hashes_active_package_sources` records one. `disabled_artifact_cache_skips_key_and_prime_render_work` records zero key/render operations for the disabled arm and one key plus two Prime renders for its one-module active control. `disabled_emit_cache_skips_context_render_serialization_and_key_work` records zero JS context renders, module serializations, and keys in both disabled entry paths, versus one of each in the corresponding active controls; cached and ordinary uncached output are byte-identical. `disabled_kio_prime_emit_cache_skips_serialization_and_key_work` records zero serialization/key operations when disabled versus one of each when active and likewise pins byte-identical output. This evidence makes no wall-time, CPU, RSS, allocation-rate, or broad-gate claim.
- **Equiv-cache shared streaming key prelude** (`cache/equiv.rs`, constructed by `cmd/test.rs`) — hashes the body-free package structure and other package-global evaluator inputs once before the parallel per-`equiv` fan-out, while each key hashes only its owner-aware reachable function-body closure; the sanitized representation streams into a fixed-size digest without materializing a JSON-shaped copy. This is perf-only. Discharge: on the `dyn_load_prime` POC's `kio test` workload, four warm samples per arm in counterbalanced `ABBA BAAB` order averaged 53.77 seconds with the owner-aware closure and 70.79 seconds with a whole-package function-body key, a 24.0% wall-time reduction. Both arms used binaries built from the same source revision apart from the key projection and identical independently warmed fixture trees; cold population took 53.63 versus 71.52 seconds. After an unrelated function-body edit, the owner-aware key retained all 65 entries while the whole-body comparator missed all 65. The focused `cache::equiv::tests` and `test_equiv_cache_warm_invalidation` golden guard shared-input coverage, function-body granularity, owner-aware reachability, deterministic registry ordering, rejection of unprojected maps, preservation of the sanitized logical tree, and sensitivity to every semantic registry field.
- **Indexed active structural-recursion sites** (`normalization.rs`) — preserves the evaluator's global per-thread LIFO order while selecting the nearest structurally equal active site through a latest-frame map and restoring it through a checked backward link. The index and links are perf-only; exact site identity, oldest-root/nearest-current diagnostics, strict descent, thread locality, and unwind cleanup are correctness-primary. Discharge (deterministic selection work, not wall time): `indexed_structural_recur_lookup_work_matches_exact_tables` records 7/15/31/63/127 actual map-selection operations at live depths 8/16/32/64/128 for both repeated and all-distinct sites, while the frozen former reverse-scan mutation records the same repeated-site row but 28/120/496/2,016/8,128 candidate visits for distinct sites and makes that exact test fail. Equal-but-distinct `Arc` keys, A/B/A restoration, every source-identity component, failed-entry immutability, thread/unwind cleanup, and private layout are separately pinned. During audit, inspect `enter_structural_recur_site`: the checked test counter must remain immediately adjacent to the actual `latest_by_site.get`, and any reverse frame scan or once-per-call synthetic counter is a finding. This evidence makes no wall-time, CPU, RSS, allocator-byte, or worst-case constant-time claim.
- **Linear recursive-signature context partition** (`pass/resolve.rs`, `sig/validate.rs`) — exact context/SCC congruence is correctness-primary. Hash-indexed binder shadowing, the signature-only duplicate-tolerant edge view, comparison-free SCC canonicalization, contiguous module runs, context hashes, the matched-member bitmap, and the ordered retained/component merge are its expected-linear realization. `recursive_context_partition_scan_fires_linearly_at_the_production_seam` drives the actual epoch validator over 128 mutually recursive declarations with 32 bound parameters each plus one trailing acyclic context member. It records 4,096 binder lookups, 129 SCC canonicalization visits, and 128 difference-merge comparisons; the former nested membership scan performs 8,384 comparisons (65.5× as many). Restoring that scan at the production selection seam bypasses the checked counter and fails the exact test. `exact_recursive_context_bypasses_the_difference_scan` accepts the corresponding complete context with the same 4,096 binder lookups, 128 SCC canonicalization visits, and zero difference-merge comparisons. During audit, reject a binder-vector membership scan, sorted/deduplicated diagnostic-edge construction on the signature-only path, comparison sorting inside `strongly_connected_components`, per-context graph analysis, or a difference-scan counter detached from the production comparison. This deterministic evidence makes no wall-time, CPU, RSS, allocation-rate, or broad-gate claim.
- **Direct recursive-component classification** (`pass/resolve.rs`, `pass/label_elab/mod.rs`) — classifies each existing SCC by its size or singleton self-edge, without retaining an index or phase state. The `cyclic_component_classification_*` tests require positive actual queries and count examined edges in label expansion, projected emission, split fixes, and diagnostics. With 128 independent cycles, those paths inspect 256/128/128/256 edges instead of the former 8,384/8,256/8,256/16,512 candidate comparisons. Complete partitions, fixes, diagnostics, mixed recursion, and projected owner self-edges remain independently checked. During audit, inspect all four callers and require each separately restored list scan to fail its work guard; reject counters detached from actual queries or singleton-edge traversal. Counter-free default-feature debug measurements used sizes 32/64/128/256, 60 repetitions per entrypoint, and four samples per arm in `ABBA BAAB` order. All 7,680 results matched complete outputs across arms. At 256 members, median-of-run-medians time fell by 70.00% for projected emission, 79.64% for split fixes, and 78.75% for diagnostics. Label lowering's aggregate 8.61% decrease was noisy under contention: paired old/fixed ratios ranged from 0.508 to 1.173, and the 32-field aggregate was 3.80% slower, so no stable label wall-time win is claimed. The measurement summary SHA-256 is `1826ab493c2c458057bffd0cacc8196e6606d8ff0ece8253dc4d2c4fa5238f95`; these are entrypoint measurements, not release-profile, end-to-end, or broad-gate evidence.
- **Ordered recursive-group recovery correspondence** (`pass/recover_to_low.rs`) — pairs source and phase-rebranded recursive-type members through the converter's one-for-one order, checks cardinality, variant, name, and binder preservation as internal phase contracts, and unfolds each newtype payload under only its own declaration binders. Direct pairing is perf-only; exact source ownership and payload unfolding are correctness-primary. `wide_recursive_group_pairs_each_distinct_source_binder_once` builds 512 mutually recursive newtypes with distinct binders colliding with outer aliases, verifies every resulting name, binder, next-member payload, and exactly 512 correspondence visits, and fails under both a one-member source-pair rotation and a wrong-member binder environment. The reproducible ignored measurement uses one deterministic 5,000-member package and five lowering iterations. Four counterbalanced samples per arm, after one discarded warmup, compare the direct pairing with the former per-newtype name scan: median wall falls from 3.635 to 1.595 seconds (56.121%), median user-plus-system CPU from 3.685 to 1.645 seconds (55.360%), median peak RSS from 106,392 to 99,222 KiB (6.739%), and correspondence visits from 62,512,500 to 25,000 (2,500.5× fewer). All eight samples exit zero and produce the identical 5,491,996-byte Routed rendering with FNV-1a `f7c6fac31253aede`. The retained measurement summary has SHA-256 `5a5fd933bd96420d0af29a015b59494850a7deaf1d302f48329bac544a512d67`; no index, cache, serialized state, or broad-gate credit is claimed.
- **Exact nominal-scope/provider acceleration and persistent alias contexts** (`pass/resolve.rs`, `pass/typecheck_core/{aliases,persistent_exact}.rs`) — indexes each module's written type-shaped selective and qualified type-import edges once, shares present indexes across phase-preserving clones, caches exact provider/owner results for one call, extends alias lexical/alpha/frontier state through structurally shared exact nodes, memoizes the free-rigid-name summary of each exact selected virtual view, and threads one incremental alpha allocator across nested alias owners. Resolution and capture hygiene are correctness-primary; the optional index, lazy provider caches, and persistent representation are perf-only realizations of that authority. Discharge (deterministic semantic-operation counts and layout, not wall time): `top_level_scope_common_header_is_one_word_over_declarations` bounds the common scope header to the declaration-map header plus one word; `top_level_scope_uses_optional_shared_type_import_storage` records no index for empty or value-only imports and one shared index across clones; and `selective_type_index_shares_one_deep_provider_prefix_across_fanout` retains exactly 64 prefix segments for a 64-segment, 64-name import. `direct_nominal_lookup_uses_top_level_scope_without_declaration_scan` records zero re-export work across 128 modules. For a 64-leaf alias output, `canonical_alias_output_uses_package_scope_without_rescanning_owner_declarations` records zero declaration-item and use-edge scans despite 64 unrelated declarations and 64 unrelated imports, while `one_alias_output_reuses_its_exact_owner_entry_handle` permits at most two exact-entry lookups. The 32-deep qualified and selective fanout controls each permit at most one exact-target lookup, one indexed-prefix clone, and 32 cloned path segments. The 64-leaf canonical alias-free composite records zero materializer roots and zero argument-rope walks; the 512-binder frontier records zero ambient-binder copies. The depth-128 capture-avoidance control records one formal-summary build, at most 256 alpha-candidate probes, and at most 768 materialization calls; the rope and forwarded-owner controls bound their counters linearly in the declared depth, arity, and width while preserving the exact materialized type. No valid timed matched A/B result is available; this discharge makes no wall-time, CPU, RSS, allocation-rate, or performance-neutrality claim.
- **Prepared boundary-facade and Python representative/annotation indexes** (`backends/boundary_facade.rs`, `backends/python/stub.rs`) — builds the live nominal/callable indexes once per facade transaction and the exact live public-newtype representative map once per Python stub render instead of rescanning package items or callable sites per boundary occurrence. Protocol localization parses and walks each emitted annotation once for all captured roots, then rewrites every exact root token simultaneously; TypedDict parameter selection indexes the current candidates once and scans the current field inventory once per instantiation, while later instantiations recompute their concrete application arguments rather than reusing an argument list cached with the definition. These realizations are perf-only. Discharge (deterministic operation counts, not wall time): `live_collection_indexes_declarations_once_instead_of_rescanning_per_site` measures width 8/32 export work at 324/4,752 legacy item visits versus 48/192 indexed visits, and both-public-newtype work at 344/5,216 versus 24/96. `public_newtype_representative_index_scales_with_rendered_width` renders width 8/32 stubs, proves representative-identity parity and exactly one construction visit per prepared site plus one lookup per public newtype, and measures 36/528 legacy early-exit site visits versus 24/96 indexed construction-plus-lookup operations. `protocol_localization_batches_mixed_exact_roots_and_preserves_ordinal_holes` preserves mixed variance, overlapping exact identifiers, and the original ordinal hole while recording 3 annotation parses, 8 AST-node visits, and 7 rendered-identifier visits; restoring the former per-root parse/walk and replacement loops records 9/24/14 and makes the exact test fail. `typed_dict_selection_visits_current_inventory_once_per_instantiation` preserves two distinct same-shape argument inventories across one cached definition and records 32 candidate visits plus 544 identifier visits for two width-16/depth-16 instantiations; restoring the former per-candidate early-exit scan records 4,624 identifier visits and makes the exact test fail. Its empty-candidate control records zero work; removing the no-op guard records two identifier visits and makes the test fail. A temporary natural width-8/32 Python pair produced 18,250/178,720-byte stubs with maximum lines of 568/1,912 bytes and square-bracket nesting 1. The former and batched implementations emitted byte-identical `.py` and `.pyi` artifacts at both widths. Four times the authored width produced 9.79 times the `.pyi` bytes because the package-root generic inventory repeats across generated declarations; that output-shape growth is unchanged by this batching and is not presented as linear. Strict Pyright 1.1.411 passed four counterbalanced samples per width: width 8 used 0.72/0.45/0.41/0.47 seconds and 122,724/122,788/122,048/122,260 KiB peak RSS; width 32 used 0.51/0.49/0.48/0.49 seconds and 127,748/127,100/127,032/127,220 KiB. Those timings are natural-scale validation only; the deterministic operation counts are the causal measurement and give no broad-gate runtime credit.
- **Backend-local prepared owner, reachability, and pending-work indexes** (`backends/rust/emit.rs`, `backends/java/{skin,emit}.rs`, `backends/swift/{skin,emit}.rs`, `backends/ts/emit.rs`) — Rust threads the exact indexed newtype-payload owner into nested forall naming rather than rescanning nominal declarations by plan pointer, Java reverses exact prepared newtype-payload plan identity to its selected nominal owner, Swift collects the exact reached-nominal set once before selecting retained host bindings, and TypeScript consumes recursive structural declarations from a pending ordered set rather than rescanning the complete discovered set after every emission. These are perf-only. Discharge (deterministic semantic-operation counts, not wall time or a claim that the underlying ordered collections are O(1)): `nested_retained_payload_foralls_resolve_their_owner_in_bounded_steps` proves identical emitted owner identity with 0/32 unrelated declarations and measures 1/1 direct semantic lookups versus 1/33 former candidate visits; `payload_owner_reverse_index_bounds_structural_operations` proves selected owner and plan-pointer parity at widths 8/32 and measures 36/528 former candidate visits versus 16/64 index-construction-plus-lookup events; `reached_nominal_index_bounds_structural_operations` proves exact-QTN selected-binding parity and measures 100/1,552 former candidate visits versus 24/96 index-construction-plus-lookup events; `recursive_structural_worklist_eliminates_ordered_rescans` proves identical ordered selection for 128 declarations and measures 8,384 former candidate extractions versus 128 pending-set extractions. Restoring each former scan at its production selection seam makes its focused guard fail while leaving the parity oracle intact, so the four indexes have causal firing coverage rather than comparator-only measurements.
- **Shared annotation/header planning and persistent lexical view** (`pass/typecheck_core/{annotation_plan,persistent_exact,kind_scheme,apply}.rs`, `prime/typer.rs`) — the one pre-mutation plan and its exact source-occurrence relation are correctness-primary. Borrowing a stack-local lexical slice, promoting one persistent exact-name prefix for movable plans, sharing that prefix among siblings, and suppressing transport state on canonical non-alias inputs are perf-only realizations. Discharge (deterministic semantic-operation counts and firing, not wall time): `borrowed_view_is_allocation_free_and_finds_the_innermost_shadow` records zero promotions, binder scans, and persistent roots for one borrowed lookup. With depth 128 and 128 sibling views, `one_promotion_is_shared_by_all_sibling_views` records exactly one promotion, 128 scanned bindings, one persistent root, and 128 lookups. `closed_header_kind_validation_reuses_borrowed_signature_binders` records one classification with zero transport materializations, rejections, committed materializations, goals, binder publications, lexical promotions, scans, or persistent roots; `ordinary_materialization_policy_has_zero_transport_storage` pins the ordinary transport sink, node, and child storage to zero size. At depth 64, `nested_target_spelling_alpha_allocation_is_linear` records exactly 128 fresh-name probes and one visit per protected-name input node; at binder depth 32 and value width 64, `wide_expected_group_installs_one_shared_alpha_prefix` records exactly 32 alpha-edge installations. The deep and wide expected-shape controls each canonicalize once and bound combined input work to eight times the respective depth or width. Lowered check/synthesis, ordinary retained/expected-lambda, and Prime check/synthesis/immediate controls each activate exactly their named policy once. The immediate-decline control records one classification and one transport materialization, zero committed materializations, goals, or binder publications, and unchanged planning state when the exact plan transfers to the computed route. No valid timed matched A/B result is available; this discharge makes no wall-time, CPU, RSS, allocation-rate, or performance-neutrality claim.
- **Empty-hole completed-argument equivalence** (`pass/typecheck_core/apply.rs`, exercised through `prime/typer.rs`) — completed arguments still require final equality. When the application-hole parameter and substitution sets are both empty, a successful ordinary identity-aware equivalence check returns without constructing, cloning, normalizing, or reconciling the generic hole-solver transaction. A mismatch falls through to the unchanged structural unifier so its leaf diagnostic and expected-source span remain stable; every nonempty state also retains that solver. This is perf-only. Discharge: on a fresh exact `dyn_load_prime` POC workdir with semantic caches disabled and one Rayon thread, the dev-profile compiler fell from 150.18 to 138.96 seconds wall (7.47%) and from 149,220.618 to 137,842.226 milliseconds frontend typecheck (7.63%); peak RSS changed from 613,236 to 615,484 KiB. Both arms exited zero with identical deterministic frontend workload counts. The earlier matched internal phase probe independently found all 109,352 explicit Prime calls and 150,809 finalized arguments in this workload had empty hole state and spent 25,862.136 milliseconds in completed-type equality. `empty_application_holes_use_direct_completed_type_equivalence` causally pins firing, identity and transparent-alias success, and scalar/nested mismatch diagnostics; restoring the old unconditional solver makes its firing assertion fail. `application_holes_keep_completed_type_solver` pins the nonempty-state no-op boundary and learned solution. The two `explicit_prime_argument_*` tests pin path, alias, literal, and checked-lambda success plus independently synthesized and dependent mismatch spans/messages. This focused measurement gives no broad-gate credit.
- **Empty-substitution semantic-call handle reuse** (`pass/typecheck_core/apply.rs`) — selection reuses an immutable interned solver, written-diagnostic, or result handle only when its substitution map is empty and the computed identity-canonical bit exactly equals the handle's existing bit. Nonempty substitutions and provenance downgrades retain the former substitution and reconstruction path. This is perf-only. Discharge: on exact base `eccbf9c5307220c27e072b5daefbf2636ce5b30e`, the dev-profile control and candidate ran fresh writable copies of Git tree `26e15e63932c55e9cda69e17ba74218f3f16b6dd` with semantic caches disabled, one Rayon thread, and two warmups followed by counterbalanced `ABBA` measurement. Median wall fell from 110.585 to 108.535 seconds, a 2.050-second (1.854%) reduction; both adjacent candidate/control savings were positive at 2.300 and 1.800 seconds. Median frontend typecheck fell from 109,376.2580 to 107,502.2585 milliseconds, a 1,873.9995-millisecond (1.713%) reduction; paired savings were 2,293.755 and 1,454.244 milliseconds. Median user CPU fell by 2.000 seconds, while median peak RSS rose by 156 KiB (0.025%). All four arms exited zero with empty stdout, identical deterministic frontend workload counts, unchanged OOM-kill count, and the frozen memory, swap, and disk floors satisfied. `semantic_call_empty_substitution_reuses_only_unchanged_provenance` pins all four `(source identity, enclosing identity)` cells at the production `select` seam: canonical solver/result and noncanonical written handles share only when the output bit is unchanged, while a planned-layer downgrade produces false identity without requiring a distinct structural `Arc`. Removing the fast path made this exact test fail on its pointer-reuse assertion before restoration. `semantic_call_cursor_substitutes_width_and_advances_one_successor` pins the nonempty-substitution path through its changed selected width. This focused measurement gives no broad-gate credit.
- **Goal-free retained-publication validation proof** (`pass/typecheck_core/goals.rs`, transported by `pass/typecheck_full/publication.rs`) — the first close and every goal-bearing retained close still perform owner/scope and goal-usability checks, canonicalization, zonking, function-scheme validation, kind checking, and publication-escape validation. Only a final identity-canonical, goal-free result mints the private move-only proof. Its intrinsic key is the owned exact `ScopedType` and rigid scope (which carry store, package-analysis context, module, and binder identities), the sealed publication root kind, and the retained output's exact next destination. A later hop still checks the current store/context, live delta and owner lifecycle, direct-parent/root destination, source-scope extension over the current owner, extension over the new destination, source order, prepared revision, and atomic commit. It skips the already-completed sealed-value walks for scoped goal-chain and goal usability, canonicalization, zonking, function-scheme shape, kind, and publication escape. This is not a reusable cache: before the proof exists, every goal/policy/delta revision stays authoritative through the old path; after no goals remain, later writes cannot change the owned type, and the enclosing publication plan consumes the proof once. The proof is Lowered-only transient state and grants no serialized, cached, Prime, reflection, or producer-provenance authority. Discharge: on the same fresh exact `dyn_load_prime` POC input used by the preceding completed-argument measurement, with semantic caches disabled, one Rayon thread, and the dev profile, current main took 138.96 seconds wall / 137,842.226 milliseconds frontend typecheck / 615,484 KiB peak RSS; the retained-publication proof took 114.71 seconds / 113,793.844 milliseconds / 614,940 KiB. That is 24.25 seconds (17.4511%) less wall time and 24,048.382 milliseconds (17.4463%) less frontend typecheck time, with 544 KiB lower peak RSS; both arms exited zero on byte-identical input and the candidate produced empty stdout. The preceding attribution measured 146,723 unique payloads, 1,556,645 close occurrences, and 1,401,605 already-goal-free retained occurrences. Restoring unconditional validation makes `deep_publication_closes_payloads_without_rewalking_command_shape` fail with 2,080 full validations and zero proof reuses instead of 64 and 2,016. `prepared_publication_is_inert_until_atomic_commit` pins the first-close no-op; `goal_bearing_retained_publication_validates_until_solution_then_reuses_proof` pins full validation through the solving close and reuse only afterward; `noncanonical_retained_publication_validates_until_canonical_proof_is_minted` and `goal_free_retained_publication_rejects_an_opposite_root_kind_before_reuse` are mutation-live for the two proof-mint guards. The function-scheme alias, child-rigid, wrong-owner/store, stale-prepared, non-function, and failure/atomicity publication tests pin canonicality, exact authority, diagnostics, and mutation boundaries. This focused measurement gives no broad-gate credit.
- **Deferred-tail classifier arena and scoped aliases** (`pass/typecheck_core/apply/fills.rs`) — candidate topology and lexical shadowing are correctness-primary. Copyable symbol handles into an invocation-local pair arena, per-role pair marks, and insert/restore alias bindings are perf-only realizations that avoid expanding shared pair topology or cloning the live alias map. Discharge (deterministic semantic-operation counts and firing, not wall time): `deferred_tail_classifier_work_is_linear_in_alias_depth_and_scope_width` builds a 12-deep doubling pair-alias chain and a 64-wide alias environment with both `either` branch binders plus 64 nested function scopes. The former owned-tree representation performs 24,547 symbol-clone visits and 4,095 pair-mark visits on the pair fixture; whole-map scope snapshots copy 4,290 alias entries on the scope fixture. The arena representation records zero owned-symbol clones, zero alias snapshots, exactly 12 pair allocations and 12 Escape-role pair marks, a linear recipe walk, and at most 264 authored binding updates while preserving terminal-versus-escape classification through nested shadow restoration. No wall-time, CPU, RSS, allocation-rate, or end-to-end performance claim is made.
- **`SharedRoutedPackage`** (`cmd/build.rs`) — computes the route + capability passes once and shares the result across a multi-target build via `OnceLock`. This is **correctness-primary, not perf-only**: the up-front `routed.get()` force (gated on `>1` selected target) averts a rayon-pool deadlock that a lazy `get_or_init` triggers when parked inside the parallel per-target dispatch. The compute-once sharing is a behavior-preserving side benefit of that force. Discharge: the force is load-bearing (removing it deadlocks multi-target builds — a hard failure, not a silent regression), so no separate perf measurement is required; a rot to per-target recompute would still be correct, only slower.
- **Runner compile profiles** (`ci/infra/kio-test-runner-rs/src/shared/opt_profile.rs`) — the per-backend test runners expose selectable `unoptimized` / `default` / `optimized` optimization profiles (`--profile` / `KIO_TEST_RUNNER_PROFILE`). `default` preserves each backend's current cheap level (rustc `opt-level=1`, but ghc `-O0` / swiftc `-Onone` — their mild tiers cost real compile time, unlike rust's near-free `opt-level=1`), and `optimized` is the on-demand higher tier (rustc `opt-level=2` / ghc `-O2` / swiftc `-O`). These **change observable output** — a different opt level is a different compiled artifact, and the profile is folded into the profile-keyed artifact cache — so they are **not** perf-only and need no separate measurement. The one perf-only lever they invited — threading rustc `-C codegen-units` on the `optimized` profile, where codegen dominates more than at `opt-level=1` — was **re-measured and rejected**: on the largest emitted crate (`dyn_load_prime`, cache-miss, several iterations) codegen-units 1 vs nproc is a wash (~1.00x) at both `opt-level=1` and `opt-level=2`, because rustc's front end is single-threaded and codegen stays a small fraction even at `-O2`. It is therefore not threaded on any profile. Discharge: no perf-only complexity remains to justify; this bullet is the recorded negative result so the codegen-units lever is not re-added on the assumption it helps.
- **Native resource engine and adaptive compiler feedback** (`ci/infra/kio-ci-scheduler-rs/`, reached through `ci/schedule.sh`) — one Git-common Rust state machine owns work, optional capacity-one Cargo, and compiler claims; the shell remains a facade. Immutable leases, transition markers, inherited Unix descriptors, Windows suspended spawn/Job supervision, isolated readiness, fixed-capacity minima and FIFO are correctness/portability boundaries. Best-effort feedback is a performance mechanism, not a universal OOM guarantee: one bounded transient record belongs to the existing lock; only the valid FIFO head advances it; fixed-only activity bypasses it; idle and failed probes retain throttling; corrupt, stale, incomparable or unwritable state falls back conservatively. Inspect the native adapters' units, available CPU scope, memory estimate, failure paths and bounded cost. Clients honor every live adaptive CPU ceiling and explicit fixed minimum. The injected policy/store tests in § 7 require growth above two, no growth without demand/headroom, pressure backoff without holder cancellation, cadence shared across heads, reset, fixed-only bypass, and current-client CPU and fixed-capacity minima. Artificially restricting the feedback target at the actual admission seam must fail the unchanged growth/admission guard. Require recorded workload evidence with exact source/tool/seed/cache identities before crediting a throughput win; classify matching, contention and uncertainty explicitly and limit causal claims accordingly. The ordinary no-override path must actually exercise feedback. Record probe/fallback behavior and aggregate resource pressure, not only largest-child RSS. Platform-independent pure tests do not establish native macOS/Windows probe or kernel behavior. The v3 trace stays outside the state lock, uses `adaptive-cap`, and emits null rather than invented targets when feedback was not consulted. Existing trace, lease, readiness, native self-test and process-tree controls remain authoritative. Keyed bootstrap reuse and inherited readiness-hook reuse retain their separate discharges: cold Linux prepare measured 2.19 seconds / 208,052 KiB versus about 0.31 seconds for warm resolve, and canonical inherited re-entry invokes zero readiness children rather than one. Those measurements give no credit to feedback.
- **Standalone Cargo compiler-only admission** (`ci/cargo.sh`) — a top-level Cargo command takes the optional scheduler-native capacity-one `cargo` resource and one compiler permit without also taking `work`. Cargo invoked by an already-scheduled corpus worker retains that inherited work lease. This prevents a queue of independent top-level Cargo commands from blocking admission of corpus work while preserving `work -> cargo -> compiler` order. This is perf-only. Discharge: in a counterbalanced `ABBA` measurement using fresh equal-shaped targets, an actual independent test-runner build overlapped the golden reduced-Prime prebuild. Total wall time fell from 36.862/36.855 seconds with the former barrier admission to 29.333/29.609 seconds with compiler-only admission (20.0% by arm medians); a waiting one-slot work marker fell from about 36.626 seconds to 0.055/0.045 seconds. Summed process-tree CPU was 70.83–71.56 seconds with the former admission and 68.63–68.92 seconds with compiler-only admission; trial-wide aggregate peak RSS was not measured. A sampled broad gate with the same seeds and `--jobs=8 --compiler-jobs=2` completed 53/53 tasks in 3,512 seconds versus a retained 4,482-second baseline; because sampled implementation assignments can differ, this broad result is integration and critical-tail evidence rather than the causal comparison. Rust tests in `resource_admission`, `held_resources`, and `compiler_admission` pin concurrent resource policy, canonical nesting order, inherited-lease retention, and scheduler re-entry; `schedule-entry-selftest.sh` pins only shell-facade dispatch, state discovery, argument/stdin transport, explicit bypass, and Cargo opt-in plumbing.
- **Persistent corpus-tool Cargo targets** (`ci/checks/orchestrators/lib/common.sh`) — exactly four corpus orchestrators reuse dedicated worktree-local Cargo targets for the identical Prime-only compiler and grammar-verifier builds. Every normal caller still invokes Cargo, whose fingerprints remain the sole staleness authority; one explicit generic cargo lease spans build and private copy, and `ci/cargo.sh` adds compiler without giving standalone helpers a work claim. The scheduler bypass retains invocation-private targets. This is perf-only. `test_stable_corpus_tool_target` pins one actual fake construction from four normal Cargo invocations, exact stable-target identity, scheduled `work -> cargo -> compiler` and standalone `cargo -> compiler` order, copy under cargo admission into four private space-bearing paths, and the bypass no-op boundary. Replacing the stable normal target with invocation-private targets or removing explicit cargo admission makes the focused self-test fail.
  Discharge: on exact compiler base `e04c9ccc5ba7be34efa24e510b313d30258f367b`, four cold-target arms compared four concurrent former private-target builds with four concurrent production helper calls in counterbalanced `A1/B1/B2/A2` order. Every arm used four work slots, two fixed compiler producers, default Cargo incremental behavior, no whole-Cargo serialization variable, and the same Prime-only `kio-prime` command. No compiler wrapper was configured and every arm recorded a zero sccache-stat delta. Median wall fell from 83.530 to 40.490 seconds, saving 43.040 seconds (51.526%); both adjacent pairs improved by 51.633% and 51.423%. Median user-plus-system CPU fell from 317.405 to 83.460 seconds, saving 233.945 seconds (73.706%); median maximum RSS fell from 2,799,768 to 2,772,182 KiB. At each cold arm's end before cleanup, median allocated Cargo-target storage fell from 6,088,085,504 bytes across four control targets to 1,521,948,672 bytes in the persistent candidate target (75.001%), while all 16 private copies were 203,201,304 bytes with SHA-256 `aaffe40b188befa1223c4576d9083ded5b7b1f970557f99caa58d4c4f415c3af` and every caller exited zero. Candidate arms also recorded negligible memory pressure, no live swap-out, and no cgroup OOM-counter change; control and candidate receipts include per-second CPU/memory/I/O telemetry. The measured production diff had SHA-256 `4e0a0305bbe60da5e7fb3d5778d6b43c185144c334fbbdee0f6d565e1e28c3eb`; the later input-validation-only rejection of exact `.` and `..` keys does not fire for the measured `kio-prime` key and leaves its target, build, and copy path unchanged. This focused build-and-copy measurement gives no broad-gate credit.
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
  Discharge: on the same 1,089-golden fake-compiler fixture, setup before worker
  dispatch fell from 33.26s to 10.87s for one implementation, from 171.85s to
  6.29s for `SAMPLE_IMPL` over eight implementations, and from 95.93s to 4.85s
  for the exact eight-implementation matrix. Under matched process tracing,
  process-creation calls fell from 22,349 to 1,161 (94.8%). A follow-up made
  target indexing consume only already-filtered cases: on a 1,095-case,
  1,857-manifest, 43,661-file golden tree, an anchored exact-filter zero-unit
  run fell from a median 11.095s to 10.585s wall and from 11.760s to 11.225s CPU
  across eight counterbalanced warm samples per arm (4.6%); the corresponding
  full-corpus target-index microstep changed from 0.297s to 0.355s per pass, so
  a more complicated hybrid path was not justified. The shared-inventory
  measurement compared against the earlier case-marker traversal's one
  subprocess per case. Four warm `ABBA BAAB` samples over the natural
  1,218-case / 1,194-package golden tree measured the old case traversal plus
  sort at median 7.195 seconds wall and 7.165 seconds user+system CPU, versus
  0.310 and 0.305 seconds for shared discovery, classification, and case
  sorting (95.7% lower for both); median peak RSS changed from 8,316 to 8,492
  KiB (+2.1%). The shared discovery emitted 319,511 bytes. This microstep
  excludes the subsequent per-selected-case workdir/sentinel probes, bounded
  awk join, and manifest target parsing and gives no broad-gate credit; the
  retained timing table has SHA-256
  `c5f9ba006f6bf7d4a72a7d81c862a7ee625f121926257e05cbbfc29072efc207`.
  A matched production-path check then compared exact base `feab3d91b` with
  candidate `2418c810e` over the same full corpus while selecting one exact
  standard case and one fake compile-only implementation. After one warmup per
  arm, four `ABBA BAAB` samples measured the complete harness invocation at
  median 8.690 seconds wall and 8.825 seconds user+system CPU for the old path,
  versus 1.965 and 2.055 seconds for the candidate (77.4% and 76.7% lower).
  Median peak RSS changed from 46,466 to 46,566 KiB (+0.2%), and all eight
  output logs were byte-identical. This setup-and-one-no-op-unit measurement
  gives no compiler, runner, or broad-gate credit; its timing table has SHA-256
  `548fc766ec0871ca74a1de208dd256e21bcc545b51395f03a5c4c41fc8ef0dc3`.
  `run-tests-predispatch-selftest.sh` pins the preserved worklist contracts,
  the exact missing/symlink/nested/multiple/workdir-symlink package
  classifications, pre-dispatch enforcement for a compile-only standard row,
  generated-descendant/out/target/hidden pruning, discovery-root marker
  non-pruning, standard package-identity transport, custom and test-only
  exemptions, and the exact two corpus/final-marker `find` processes.
- **Exact-name selector batching** (`ci/run-tests.sh`) — conservatively classifies a complete positional-filter set as tightly anchored safe case names, matches that set with one `grep -E -f` pass, and otherwise retains the legacy per-case/per-selector matcher unchanged. Exact original case paths, discovery order, exclusions, and Prime/dynamic marker gates are preserved; classifier, matcher, staging, or remapping anomalies fail closed to the legacy path. This is perf-only. Discharge: on a 1,089-case fixture with 32 anchored selectors and the sole hit last, four measured samples per arm after one discarded batched warmup in counterbalanced `ABBA BAAB` order produced median wall times of 88.070 seconds for the legacy loop and 6.285 seconds for the batch path (92.9% lower), with `min(legacy)=87.518` greater than `max(batch)=6.441`. Matcher invocations fell from 34,848 legacy comparisons to one batch process. Every arm had byte-identical stdout, stderr, status, and selected-case output, and tripwire tools proved that no case compiler or runner work executed. Scaling only the separated lower bound and pessimistically debiting all measured batch time as pattern-scan growth gives a conservative 341-selector saving of 801.774 seconds after a 62.198-second debit. Unfiltered runs bypass this optimization, so the measurement gives no broad-gate credit. `run-tests-predispatch-selftest.sh` pins exact-path/order parity, safe-classifier firing and no-op behavior, general-regex fallback, clean no-match handling, diagnostic/partial-output fallback, Prime/dynamic/exclusion interactions, bounded matcher processes, path forms, and mutation-live causals.
- **Golden Kio' roundtrip multi-target batching** (`ci/checks/per-case/kio-prime-roundtrip.sh`) — combines the producer compiler's direct-target and Kio' builds for expected-success roundtrip rows. When the combined transaction fails, the production check replays the two legacy builds fail-closed, preserving target-inapplicability skips, failure attribution, and byte-for-byte output comparison. This is perf-only and preserves golden behavior and output. Discharge (deterministic operation counts): on the fixed cohort of 32 regular, 3 Prime, and 4 dyn-load-prime executions, its 35 regular + Prime roundtrip rows reduced producer compiler transactions from 70 to 35 and total roundtrip compiler invocations from 105 to 70, including unchanged reduced builds. This evidence makes no wall-time, CPU, RSS, latency, or O(1) claim. `ci/checks/repo-lint/kio-prime-roundtrip-batching-selftest.sh` is the causal firing guard; it pins the combined request, fail-closed fallback, inapplicable-target skip, failure attribution, independent verifier, and byte-for-byte comparison.

- **Streaming and reusable dynamic-loader declaration scopes** (`test-data/poc/dyn_load_prime/workdir/loader.kio`) — resolution streams the unprocessed suffix and reversed processed prefix rather than reconstructing both from a numeric position. It rebuilds the complete declaration scope for the first declaration of a module, then advances and reuses that exact scope for consecutive declarations in the same module. An owner or cutoff discontinuity, or a source-local host function becoming visible between the two declarations, falls back to the complete rebuild. Scope contents, declaration/error order, source-order visibility, and the evaluator's de Bruijn chain are unchanged. This is perf-only. The streaming-prefix step was measured on a frozen 215-declaration image: matched `ABBA` medians fell from 133.77 to 131.87 CPU-seconds and from 133.80 to 131.88 seconds wall (1.42% and 1.43%), while removing 46,225 source-level list visits; its tiny-image control showed no material change. The later reusable-scope step was remeasured on exact base `8aefebce8b163f8c524a34e6927c09eae24b9c1f` with one frozen `exec_show_cross_package_host_type` Kio' image, one compiler-built pair of driver artifacts, one runner binary, and fresh serial runner processes in candidate/control/control/candidate order. Controls took 205.57 and 205.95 seconds wall; candidates took 200.43 and 202.47, so medians fell from 205.76 to 201.45 seconds, saving 4.31 seconds (2.094%). Corresponding user-plus-system medians fell from 205.725 to 201.430 seconds; peak RSS was 33,388--33,588 KiB. All four arms exited zero, reported zero swaps, emitted identical stdout SHA-256 `bb1b98251cee0a50b9b71c545964dbc1a51517610e5b161e465bff2dcbc2146d`, and had empty stderr. The frozen image-manifest SHA-256 is `9d3c6afacb84f8cf5c89af8ed85cfc28e2ecbf814949fa791c773ba829c7586f`; the retained measurement summary SHA-256 is `f5fa09a89c3c45a9de2c6e55d8e37264a3d420e0d1cb732bb0eeb5d550acb25d`. These focused measurements give no broad-gate credit. The natural 215-declaration workload fires same-module reuse; the POC's existing single loaded image now places required `add_i32` between private `scope_before` and `scope_after`, pinning the invalidation fallback without changing the public output or one-load contract. Its private architecture guard pins the reuse and fallback wiring against becoming dead scaffolding.

- **Exact public bridge scope for scripted dynamic-load hosts** (`test-data/goldens/00_success/exec_dyn_load_*`) — each custom host builds the complete copied loader/interpreter package but exposes only the root `testapi` module and the seven submodules used by its fixed runner protocol. Internal implementation modules remain ordinary imports and continue through typechecking and emission; runners that invoke `kio test` retain their package `equiv` checks. Only unused generated host facades disappear. This is perf-only. The original one-case discharge used exact base `34b723e3939db0bf17d46c0864b9a168ae2d7d18`: three runner-cache-disabled control trials were interleaved with a conservative narrowed candidate before three immediately following trials of the final exact-eight `exec_dyn_load_exact_descriptors` manifest. Every control and exact-final trial ran that one anchored Haskell golden with the same worktree binaries, GHC `-O0`, and the native compiler observer; all six passed the runtime golden and invariants, every control emitted SHA-256 `959623fabe30e07d16876ef803bc0154ab9b5439254888beb3a0aa64b5b42169`, and every exact-final trial emitted SHA-256 `30a0aeefd22bd426ccc8fb36c9a60edd3b57b40b13784e5c42c0ce2f94ef1902`. Median whole-case wall fell from 532.98 to 205.63 seconds (61.419%), and median summed user-plus-system CPU fell from 753.29 to 415.28 seconds (44.871%). Median GHC wall fell from 385.83 to 64.61 seconds (83.254%), median GHC CPU from 387.81 to 64.87 seconds (83.273%), and median peak RSS from 5,953,156 to 3,506,336 KiB (41.101%); every compiler reported zero swaps. Generated Haskell fell from 41,445 lines / 21,961,471 bytes to 6,779 lines / 10,101,357 bytes. The control and exact-final manifest SHA-256 values are `9df613fd5fd39ff8bf4a546c910029b19dbae6ec8585a877c0cf3d25c752d6ad` and `0b75030a7c1c5a1c810d553eb4372640a2182dd5f261d0a38f699e99aa96f9f3`.

  Generalization discharge: on exact base `c13bdb6c1`, an anchored runner-cache-disabled Haskell comparison covered `exec_dyn_load_exact_descriptors`, `exec_dyn_load_host_callback`, and `exec_dyn_load_type_exports` with `--jobs=3`, `--compiler-jobs=2`, and GHC `-O0`. The control arm, with `exec_dyn_load_exact_descriptors` already exact-eight and the other two manifests still overbroad, measured 461.59 seconds wall, 1,237.52 seconds summed user-plus-system CPU, and 5,853,500 KiB peak RSS. After one discarded candidate warmup, the arm with all three manifests exact-eight measured 267.13 seconds wall, 745.92 seconds CPU, and 3,632,116 KiB peak RSS: reductions of 42.128%, 39.725%, and 37.950%, respectively. Every runtime and invariant row passed in both measured arms. These focused measurements give no broad-gate credit. `dyn-load-host-surface.sh` derives the complete scripted-host cohort structurally, validates its canonical source and fixed protocol fail-closed, and enforces the exact root bridge surface; its self-test is the causal firing guard. The unchanged fixed runner remains the independent sufficiency oracle, and the custom typed-cache cohort independently snapshots its reviewed 26-script subset.

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
  Discharge: on exact base `feab3d91bc26e2dc7b0282510f76abc2418e3fb5`
  and the natural 1,218-case golden tree, four matched samples per arm in
  counterbalanced `ABBA BAAB` order produced byte-identical sorted case paths.
  Median wall time fell from 7.050 to 0.165 seconds (97.660%, 42.7x), with the
  slowest candidate at 0.18 seconds below the fastest baseline at 6.60 seconds.
  Median user-plus-system CPU fell from 7.065 to 0.155 seconds (97.806%); median
  maximum RSS rose from 8,030 to 8,180 KiB (1.868%). The change removes 1,218
  one-file `dirname` subprocesses while retaining the same `find` and `sort`
  passes. The measured production patch has SHA-256
  `c884ac66ef714b01c7db5c4a31d4fecb3aea296757d9a097d839d8cab59c9405`.
  Measurements of earlier discarded designs give this response no credit.

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
  bounded-work assertion fail. The discovery-only comparison uses the workdirs
  of `exec_dict_fold_list_interop`, `root_module_header_minimal` and
  `build_intrinsics`, in that order under `test-data/goldens/00_success/`.
  Baseline commit is `3504053d874b07cd92c1db700b9a0a39f87eeb5d`; measured
  production-script SHA256 is
  `8a7b52745a7bcd7c766e5109f04492f1062e86f1119f4fc3d28218c52d44c205`.
  With one warmup per arm and four counterbalanced `ABBA` samples, each sample
  repeats those three workdirs 16 times (48 discovery transactions). Median
  wall was 10.270 versus 2.065 seconds (79.89% lower), user-plus-system CPU
  10.835 versus 2.290 seconds, and peak RSS 11,904 versus 11,938 KiB. Every
  sample returned zero with byte-identical ordered candidate lists and empty
  stderr. These are exact discovery-prefix timings, not compiler, verifier,
  whole-check or broad-gate savings.

For each registered optimization, discharge both:

- **Firing coverage?** Use the narrowest truthful evidence. Observable cross-implementation language/CLI/runtime behavior is golden-pinned through the fixed runner; a golden case never reads, copies, greps, patches, imports, or native-compiles generated host-backend files. A durable generated-file proxy retained with the recorded measurement is an `ARTIFACT_SHAPE` emission, and it pins only that portable measured/spec fact. Exact private/no-filesystem firing belongs in unit or mutation coverage (`audit-mutation`). Incidental private artifact assertions are deleted rather than promoted to corpus contracts.
- **Measured win?** It landed (or is retained) with a recorded measurement — a number in the commit / PR or a benchmark fixture.

Kio' is the specified backend-neutral phase-artifact exception: a golden may
read or assemble it when the phase artifact, round trip, verifier/evaluator, or
dynamic-load boundary is the subject, and a generic harness-owned phase check
may do likewise. That exception does not authorize direct host-backend output
inspection. Emissions supplement runtime coverage and never satisfy a backend-
completeness runtime cell.

Findings:

- **Unjustified perf complexity** — a perf-only optimization with neither firing coverage nor a recorded measured win; it must be measured or removed.
- **Unregistered perf-only optimization** — behavior-preserving complexity not in the list above; add a row and discharge it.

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

This is a proportionality audit, not a fixed speed contest. Flag superlinear or
duplicated work, missing wide evidence, and resource mitigation presented as a
root-cause fix. Do not require a broad benchmark for an edit that cannot affect
backend preparation, emitted shape, or native compilation.

The Python typed-stub package is one registered correctness-primary scaling
mechanism. Check its bound and causal guards explicitly:

```sh
grep -nE 'PYTHON_STUB_DECLARATIONS_PER_SHARD|shard_python_stub|lower_package_to_stub_package' \
  kio-rs/src/backends/python/stub.rs kio-rs/src/cmd/build.rs
grep -nE 'stub_sharding_bounds_modules|missing generated declaration shard|missing shard re-export|800|KIO_MYPY_BIN' \
  kio-rs/src/backends/python/stub.rs \
  ci/checks/repo-lint/pyright-strict-selftest.sh
```

Confirm that the public entry point re-exports every deterministic private
shard, each declaration has one owner, cross-shard references retain that
owner's exact identity, and the 256-declaration ceiling has not been weakened.
The self-test must execute a greater-than-256 nominal/TypeVar cycle through
Pyright, retain the optional pinned mypy replay, and prove that large single
Protocol and TypedDict member bodies do not hide a second Pyright complexity
ceiling. Recheck the portable natural-wide and narrow measurements registered
in `ai/topics/implementation.md` when the layout, bound, or checker version
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
   fixed-depth structured-state witness.
8. **Test gap** — no focused guard for a performance-architecture change, or
   missing per-backend runtime, wrapper-growing RED/restored GREEN, or semantic
   boundary controls for recursive stack safety.
9. **Evidence misplacement** — a golden inspects generated host output, an
   `ARTIFACT_SHAPE` pins an incidental/private detail without a durable spec or
   recorded measurement, or emission evidence is credited as runtime coverage.

For each finding, cite `file:line`, explain the workflow impacted (`kio build`, warm `kio check`, LSP diagnostics, typed LSP request, docs/tests), and state the smallest corrective direction. Distinguish true findings from deliberate sequential code; the greps over-match by design.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). Fix the architecture or add missing regression guards; do not paper over a real performance regression with a comment.
