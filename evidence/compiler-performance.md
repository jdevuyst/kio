# Compiler performance evidence

Measurement and acceptance records for compiler and language-tooling mechanisms.
Test-runner, scheduling and build-preparation measurements are in
[CI performance evidence](ci-performance.md).

Historical records do not automatically revalidate the current implementation.
Each record preserves its available workload, controls, source/test anchors,
settings, results and claim limits. Some inherited summaries do not identify the
measurement's source commit, exact toolchain, full invocation or raw-log
location. An omitted identifier or setting remains unrecorded; it must not be
inferred from the current checkout. Claims apply to the stated experiment and
assumptions, not to an unspecified current toolchain or whole-gate comparison.

## Settled type slots and checked-call replay

Bounded same-binary debug probes measured type-slot medians of 8.481 → 5.780 µs for a two-leaf product and 75.106 → 44.097 µs for a 32-leaf product; stage assembly measured 2.240 → 2.016 µs for two shallow arguments and 114.302 → 62.279 µs for sixteen depth-16 arguments. These short seam measurements establish no whole-check or memory win.

Implementation and guarding tests: `kio-rs/src/pass/typecheck_core/{call_args,goals}.rs`, `kio-rs/src/pass/typecheck_core/apply/fills.rs`, `kio-rs/src/pass/substitute/{mod,checked_call_tests}.rs`, `kio-rs/src/prime/typer.rs`; exact scope/type/error, actual probe-count, owned-allocation and shared-slot guards; bounded `settled_type_slot_materialization_cost` and `recorded_call_stage_cost` comparisons.

The same-binary debug materialization and stage-assembly probes used ABBA order.
Each arm has two samples; input copying is included in both stage arms.
Process peak memory showed no win. These seam probes give no isolated cursor,
whole-compiler or CI timing credit.

## Macro artifact selection before copying

A bounded debug-profile copy probe reduces 25 body clones to one and median copy time from 1.120 ms to 0.009 ms; a four-body control reduces to one and improves from 0.016 ms to 0.012 ms. These are artifact-copy costs, not whole-check speedups; peak process memory is effectively unchanged.

Implementation and guarding tests: `kio-rs/src/pass/typecheck_full.rs`, `kio-rs/src/pass/typecheck_full/eval_module_copy_tests.rs`; exact-module/cutoff equality, actual clone counts, clone-before-filter causal, and same-binary `eval_module_copy_cost` comparison.

## Structural type interner exact-view cost

In the recorded exact-view-only measurement, which does not measure the occurrence-local requirement carrier, four counterbalanced no-cache checks of the source-heavy `dyn_load_prime` POC per arm showed that the exact-view representation changed median wall from 36.955 to 37.300 seconds (+0.9%), summed user+system CPU from 110.980 to 112.700 seconds (+1.5%), and peak RSS from 828,246 to 858,318 KiB (+3.6%, 29.4 MiB); all outputs were empty and every command exited zero.

Implementation and guarding tests: `kio-rs/src/pass/typecheck_core/{intern,memo}.rs`, `kio-rs/src/pass/typecheck_full.rs`, `kio-rs/src/cmd/check.rs`.

## Bounded Python typed-stub packages

The natural `dyn_load_prime` witness emits 139 shards plus the entry point: 26,972,007 bytes, 111,028 lines, and 35,571 declarations total, with maxima of 256 declarations, 317 top-level statements, 763,016 bytes, and 1,719 lines in one file. Pyright 1.1.411 in strict mode completes that package with no diagnostics in 37.48 seconds wall at 2,996,284 KiB peak RSS (52.09 seconds user, 3.67 seconds system, 148% CPU, zero swaps). The narrow `typed_stub_skin` control emits one shard plus the entry point, 9,164 bytes, 112 lines, and 31 declarations; the equivalent strict package check completes in 0.58 seconds at 135,320 KiB. A matched logical-flat reconstruction of the wide declarations is 23,256,903 bytes and 109,904 lines and stops at Pyright's module-complexity diagnostic after 32.61 seconds at 2,254,256 KiB, so that control establishes the need for bounded modules but makes no latency or RSS improvement claim because it exits early.

Implementation and guarding tests: `kio-rs/src/backends/python/stub.rs`, `kio-rs/src/cmd/build.rs`, `ci/checks/per-case/pyright-strict.sh`, `ci/checks/repo-lint/pyright-strict-selftest.sh`; deterministic structure, cross-shard identity, strict Pyright/mypy execution, runtime/type-view coexistence, stale-artifact replacement, and missing-shard/re-export guards are focused there.

## Exact Python TypedDict definition sharing

On the `dyn_load_prime` POC, sharing replaces 28,965 duplicate bodies while retaining all 48,681 declarations and the byte-identical runtime. Stub bytes increase from 36,941,639 to 38,368,290; this saves checker definitions, not source size. With Node 24.18.0, Pyright 1.1.411, strict Python 3.10 checking, and `--max-old-space-size=2048`, the unshared package exhausts the heap after 71.94 seconds at 2,236,824 KiB RSS; shared checks finish without diagnostics in 51.13/49.95 seconds at 2,150,880/2,140,216 KiB. A matched no-cache build pair measures backend emission at 6,068.662/6,132.383 ms and whole-build peak RSS at 1,329,408/1,380,812 KiB; the extra map is a cost, and these measurements establish checker completion rather than compiler acceleration.

Implementation and guarding tests: `kio-rs/src/backends/python/stub.rs` exact-key/no-op unit, `test-data/emissions/python/typed_dict_definition_sharing` portable definition-count proxy, `test-data/emissions/python/typed_stub_skin` strict host interface, and the exact Python `dyn_load_prime` POC.

## Shared recursive-group step

Source: `kio-rs/src/pass/desugar/mod.rs`.

`mutual_rec_dispatcher_is_emitted_once_per_group`
  guards 2/4/8-member scaling and genuinely fails when per-wrapper construction
  is restored. The eight `rec_group_shared_step` emissions measure each of five
  effect-body literals once; the old compiler emits four copies of the first
  literal on every backend and fails each guard. These are structural savings, not runtime acceptance.

## Shared literal Unit beta reduction

Source: `kio-rs/src/pass/optimize.rs`.

The independent unary
  group guard is a correctness repair, pinned by `exec_literal_beta_stage_order`,
  not performance credit. `beta_reduces_nullary_literal_with_omitted_or_explicit_unit`
  fails when the catalog route is disabled; adjacent effect/capture/stage tests
  constrain the transformation. Production emission reduces Unit-lift sites
  from 1,954 to 248 and the measured simple immediate-thunk pattern from 203
  to zero, including the hot boolean helpers. A restricted artifact-only model
  removes those 203 simple-body thunks and changes the matched lexer from
  14.726 to 12.867 seconds. The fresh combined compiler output takes 10.932
  seconds with identical complete token output; its metadata also differs,
  so this is not an isolated whole-rule timing. The exploratory artifact rewrite alone does not establish production implementation evidence.

## Demand-initialized replay requalification

Source: `kio-rs/src/pass/substitute/mod.rs`, `kio-rs/src/pass/typecheck_full.rs`.

Recorded combined result: one counter-free matched invocation per arm of
  `exec_dyn_load_exact_descriptors` with Python and unchanged invariant coverage
  takes 106.61 → 93.55 seconds wall (12.25% lower) and 237.90 → 218.88 seconds CPU
  (8.00% lower). Inputs, opt-level-1 compiler profile, default resource policy
  and fresh invocation-local semantic caches match. Peak RSS increases
  1,528,852 → 1,573,516 KiB; this is not a memory win. Actual warm-dependency
  compiler preparation is separately recorded at 140.03 and 128.90 seconds;
  those rebuilds have different source deltas and are not a preparation-speedup
  comparison. The ordered pair establishes no confidence interval, cold-build,
  per-occurrence, whole-corpus or whole-gate speedup.

## Package-scoped signature identity index

Source: `kio-rs/src/pass/typecheck_core/modules.rs`, `kio-rs/src/pass/resolve.rs`.

`signature_package_builds_one_identity_alias_index_across_modules` observes actual index construction and visited items, including independent `T = .` aliases that have no identity-alias edge: 64 and 128 modules require one build and respectively 64 and 128 item visits.  `signature_index_reuse_stays_within_one_package_check` pins fresh package invocations, fresh individual-module checks, and the empty-package no-op. `signature_package_scope_matches_fresh_module_checks` compares accepted and rejected complete results with the former independently scoped module loop, including distinct imported identities, alias/newtype dependencies, higher-kinded arguments and invalid type applications, including same-spelled aliases with distinct provider arities. No completed matched timing comparison is recorded in the inherited summary.

## Indexed fresh-signature origin slices

Source: `kio-rs/src/sig/validate.rs`, `kio-rs/src/sig/replay.rs`.

`fresh_canonicalization_work_is_linear_for_independent_declarations` observes the actual `build_package_from_modules` seam during canonicalization: 64 and 128 independent aliases require respectively 64 and 128 constructed module items and packages.  Final epoch construction collects each module's first present compile-time import in its existing declaration pass, before recursive-context deduplication. `fresh_epoch_construction_work_is_linear_across_modules` drives replay over 64 and 128 modules and counts actual declaration-iterator visits in `epoch_modules`: 128 and 256 visits across pre-removal and post-removal validation.   `epoch_modules_preserve_first_comptime_import_and_origin_override` pins first-present order, origin override including an empty list, recursive-context deduplication, and empty-module behavior. The import fixtures pin selected identities, while `fresh_origin_slice_matches_full_epoch_resolution_and_qualification` and `fresh_origin_slice_matches_full_epoch_recursive_context` compare accepted canonical declarations and rejected diagnostics with complete origin/epoch construction. Independent bounded-size declarations with bounded imports have bounded per-origin construction and one traversal per final epoch construction even when spread across modules; ordered indexes retain logarithmic factors, and these guards make no all-histories linearity claim. No completed matched timing comparison is recorded in the inherited summary.

## Imported newtype-variance memo

Source: `kio-rs/src/pass/typecheck_core/variance.rs`.

Recorded result (deterministic provider-derivation counts, not wall time): `imported_variance_memo_avoids_branching_provider_rederivations` checks the same accepted nested-contravariant payload with and without ready-result reuse. Across a five-provider chain with two references at every link, the memoized path derives `p0` through `p4` once each (5 derivations total); the test-only ready-reuse-disabled comparator derives them 512/128/32/8/2 times respectively (682 total). Removing the ready hit makes the exact count assertion fail while both arms preserve the positivity result. This evidence makes no wall-time, CPU, RSS, allocation-rate, or end-to-end performance claim.

## Macro artifact/implementation pairing and analysis reuse

Source: `kio-rs/src/pass/typecheck_full.rs`, `kio-rs/src/pass/typecheck_core/{intern,memo}.rs`.

Recorded result: on a cold `kio test --no-cache` run of the package fixture used by `exec_dyn_load_exact_descriptors`, the blocked per-module-batch implementation took 192.88 seconds wall / 796.37 seconds CPU / 1,975,008 KiB peak RSS; package-check reuse took 53.66 seconds wall / 107.35 seconds CPU / 1,961,900 KiB peak RSS (72.2% less wall time and 86.5% less CPU, with effectively unchanged memory). The latter run exercised four retained artifact identities (1,879 hits / 4 misses / 4 first writes), not a single-key microcase. On the fixed single-thread macro proxy, wall time fell from 12.51 to 4.53 seconds and CPU from 5.84 to 4.52 seconds. `user_macro_artifact_pairs_are_analysis_scoped_singleflight` pins exact pairing, capture separation, success-only caching, singleflight, and analysis lifetime; `reused_structural_interner_does_not_reuse_package_content` pins the raw-interner boundary; `package_versions_share_structural_types_but_not_content_memos_or_analysis_artifacts` pins all three ownership layers; `revision_store_reuses_only_exact_versions_and_binds_each_scope`, `lsp_user_macro_memo_survives_repeated_same_snapshot_analysis`, and `lsp_user_macro_memo_invalidates_after_provider_edit` pin revision replacement, analysis-owner binding, same-fingerprint warm reuse, and provider-edit invalidation. `lsp_missing_focus_fallback_reuses_worker_memos` pins the target-disappearance fallback to that same revision store: two identical fallback analyses record one miss and one hit rather than leaving the caller's store empty. `imported_consumer_batches_share_one_exact_artifact_per_package_check` and `eager_result_inference_and_deferred_check_share_one_exact_artifact` pin the two reuse paths; `staged_provider_helper_reconstructs_imported_surface_elaboration` pins ambient-free provider reconstruction.

Artifact-miss copying costs are recorded under [Macro artifact selection before copying](#macro-artifact-selection-before-copying); these do not establish per-call body sharing, whole-check speedups or a peak-memory win.

## Ordered cancellable focused-module force/lower fan-out

Source: `kio-rs/src/cmd/check.rs`.

Recorded result: exact `77274b4397c66d79152fad63e390128da0d99389` baseline and integrated-candidate release binaries drove fresh real `kio lsp` processes with eight Rayon threads in six counterbalanced samples per arm. On the natural `library_catalog_composite` castle focused at `testapi/main.kio`, median internal foreground analysis fell from 655.492 to 510.801 milliseconds (22.074%) and matched external request-to-analysis latency fell from 656.711 to 511.947 milliseconds (22.044%); every candidate sample was faster than every baseline sample. Median focused-completion CPU rose from 0.980 to 1.035 seconds (+5.612%), and sampled focused peak RSS rose from 189,578 to 193,478 KiB (+2.057%). All twelve analyses returned `outcome=ok` and byte-identical hover responses. Historical pre-rebase controls on exact `317aa9c91b541df4416c757b59665ba5818054fc` amplified the causal effect on a body-heavy 24-module package (3,011.491 to 1,933.115 milliseconds, 35.809%, at +2.559% sampled peak RSS), put the natural dict POC's 1.862% change within its sample spread, and bounded one-module no-material-fan-out overhead below 0.14 milliseconds in median external latency. Measurement processes were stopped immediately after the verified response so the independently debounced full analysis could not contaminate focused latency or RSS; this gives no broad-gate credit. `focused_force_collection_overlaps_work_and_preserves_input_order` is genuinely red with the serial iterator and pins both firing and ordered collection; `focused_force_collection_cancels_before_queued_work` runs through the same helper with a one-thread pool and through the no-`parallel` fallback; `focused_force_collection_keeps_first_error_in_input_order` and `lsp_focused_force_reports_the_same_first_module_error` pin deterministic private and end-to-end error selection.

## Run-scoped cross-package typed-module reuse

Source: `kio-rs/src/cache/typed.rs`, `kio-rs/src/cmd/check.rs`, `ci/run-tests.sh`.

Recorded result: two fixed 16-file, 603,268-byte golden packages (`exec_match_single_polymorphic_scalar_call` and `exec_narrow_sum_checked_term_scope`) contain 14 typed modules with 13 byte-identical semantic inputs. Four fresh-root matched pairs in counterbalanced `ABBA BAAB` treatment order, with both `XY` and `YX` package order, compared package-local roots against one cross-package root. All four pairs were directional. The measured implementation includes cycle-complete iterative reachability, exact-key plus encoded-body integrity validation, and global/package-disabled zero-work paths. The median second-command wall reduction was 23.2178% (pair gains 30.9478%, 17.5926%, 28.4810%, and 17.9545%); median two-command wall reduction was 10.8282% (12.6397%, 10.7143%, 8.5169%, and 10.9420%); median summed user+system CPU reduction was 32.2340% (33.0511%, 31.7386%, 31.5562%, and 32.7295%). Peak RSS stayed within the package-local arm plus the larger of 5% or 65,536 KiB in every pair. All 16 commands exited zero and reproduced package-matched Kio-prime output manifests; no sample was discarded or replaced. The untimed causal preflight recorded 0 hits / 14 misses / 14 writes for both package-local builds and the shared-root first build, then 13 hits / 1 miss / 1 write for the second shared-root package, whose sole miss was its distinct `testapi/main`; its package-local clear did not touch the shared root. This measurement gives no broad-gate credit. A production-path `ABBA BAAB` A/B over two exact custom dynamic-load goldens measured four fresh control invocations against four authenticated-cohort invocations. Median wall fell from 92.840 to 70.680 seconds (23.869%); all four matched savings were positive at 23.27, 21.58, 22.03, and 21.67 seconds, with median 21.850 seconds. Median matched user+system CPU saving was 75.470 seconds; every treatment/control RSS ratio was at most 1.028. Control cache evidence was 0 hits / 101 misses / 101 writes and treatment was 47 / 54 / 54, including the five distinct guest modules built by the scripts. All eight commands exited zero with identical normalized output, unchanged source/binary identities, and OOM 0 to 0; this focused measurement gives no broad-gate credit. `semantic_module_identity_reuses_entries_across_package_names`, the key-input tests, concurrent/late-publication and corrupt-destination tests, and flat-entry GC test pin private identity, publication, and storage firing. `typed_modules_reuse_one_semantic_entry_across_package_names` pins package/config/bridge/nominal/visibility/open-world and ordinary dependency invalidation boundaries; `same_package_dependencies_come_only_from_import_clauses` plus `imported_operator_callable_edit_invalidates_an_importing_consumer` pin written-import dependency authority and transitive callable invalidation; `typed_cache_debug_root_shares_only_explicitly_enabled_package_entries` and `typed_cache_debug_root_ignores_relative_paths` pin debug-root policy; `typed_cache_dependency_invalidation` pins source/dependency key movement in both frontends; `run-tests-progress-selftest.sh` pins default custom isolation, authenticated fixed-cohort sharing, frozen script bytes, and cleanup; and `coverage-policy-selftest.sh` pins regular-golden-only cohort activation.
  `every_root_of_a_three_cycle_has_the_complete_reachable_set` and `parsed_mixed_cycle_moves_downstream_fingerprints_after_member_edit` pin cycle-complete iterative reachability through ordinary and macro imports. `globally_disabled_typed_cache_does_no_dependency_or_key_work`, `package_disabled_typed_cache_does_no_dependency_or_key_work`, and `enabled_typed_cache_prepares_dependencies_and_key` causally pin the disabled no-op and enabled firing paths. `exact_key_header_rejects_a_different_valid_module_body` pins encoded-body integrity independently of semantic-key header validation; the existing concurrent, corrupt-destination, and retry controls exercise the same validated read path.

## Disabled semantic-cache zero-work gates

Source: `kio-rs/src/cache/{artifact,emit}.rs`, `kio-rs/src/cmd/{build,check}.rs`, `kio-rs/src/backends/js/emit.rs`.

Recorded result (deterministic operation counts, not wall time): `globally_disabled_package_check_cache_skips_source_hash_work` and `package_disabled_package_check_cache_skips_source_hash_work` record zero source hashes, while `enabled_package_check_cache_hashes_active_package_sources` records one. `disabled_artifact_cache_skips_key_and_prime_render_work` records zero key/render operations for the disabled arm and one key plus two Prime renders for its one-module active control. `disabled_emit_cache_skips_context_render_serialization_and_key_work` records zero JS context renders, module serializations, and keys in both disabled entry paths, versus one of each in the corresponding active controls; cached and ordinary uncached output are byte-identical. `disabled_kio_prime_emit_cache_skips_serialization_and_key_work` records zero serialization/key operations when disabled versus one of each when active and likewise pins byte-identical output. This evidence makes no wall-time, CPU, RSS, allocation-rate, or broad-gate claim.

## Equiv-cache shared streaming key prelude

Source: `kio-rs/src/cache/equiv.rs`, constructed by `kio-rs/src/cmd/test.rs`.

Recorded result: on the `dyn_load_prime` POC's `kio test` workload, four warm samples per arm in counterbalanced `ABBA BAAB` order averaged 53.77 seconds with the owner-aware closure and 70.79 seconds with a whole-package function-body key, a 24.0% wall-time reduction. Both arms used binaries built from the same source revision apart from the key projection and identical independently warmed fixture trees; cold population took 53.63 versus 71.52 seconds. After an unrelated function-body edit, the owner-aware key retained all 65 entries while the whole-body comparator missed all 65. The focused `cache::equiv::tests` and `test_equiv_cache_warm_invalidation` golden guard shared-input coverage, function-body granularity, owner-aware reachability, deterministic registry ordering, rejection of unprojected maps, preservation of the sanitized logical tree, and sensitivity to every semantic registry field.

## Linear recursive-signature context partition

Source: `kio-rs/src/pass/resolve.rs`, `kio-rs/src/sig/validate.rs`.

`recursive_context_partition_scan_fires_linearly_at_the_production_seam` drives the actual epoch validator over 128 mutually recursive declarations with 32 bound parameters each plus one trailing acyclic context member. It records 4,096 binder lookups, 129 SCC canonicalization visits, and 128 difference-merge comparisons; the former nested membership scan performs 8,384 comparisons (65.5× as many). Restoring that scan at the production selection seam bypasses the checked counter and fails the exact test. `exact_recursive_context_bypasses_the_difference_scan` accepts the corresponding complete context with the same 4,096 binder lookups, 128 SCC canonicalization visits, and zero difference-merge comparisons.  This deterministic evidence makes no wall-time, CPU, RSS, allocation-rate, or broad-gate claim.

## Direct recursive-component classification

Source: `kio-rs/src/pass/resolve.rs`, `kio-rs/src/pass/label_elab/mod.rs`.

The `cyclic_component_classification_*` tests require positive actual queries and count examined edges in label expansion, projected emission, split fixes, and diagnostics. With 128 independent cycles, those paths inspect 256/128/128/256 edges instead of the former 8,384/8,256/8,256/16,512 candidate comparisons. Complete partitions, fixes, diagnostics, mixed recursion, and projected owner self-edges remain independently checked.  Counter-free default-feature debug measurements used sizes 32/64/128/256, 60 repetitions per entrypoint, and four samples per arm in `ABBA BAAB` order. All 7,680 results matched complete outputs across arms. At 256 members, median-of-run-medians time fell by 70.00% for projected emission, 79.64% for split fixes, and 78.75% for diagnostics. Label lowering's aggregate 8.61% decrease was noisy under contention: paired old/fixed ratios ranged from 0.508 to 1.173, and the 32-field aggregate was 3.80% slower, so no stable label wall-time win is claimed. The measurement summary SHA-256 is `1826ab493c2c458057bffd0cacc8196e6606d8ff0ece8253dc4d2c4fa5238f95`; these are entrypoint measurements, not release-profile, end-to-end, or broad-gate evidence.

## Ordered recursive-group recovery correspondence

Source: `kio-rs/src/pass/route.rs`.

`wide_recursive_group_pairs_each_distinct_source_binder_once` builds 512 mutually recursive newtypes with distinct binders colliding with outer aliases, verifies every resulting name, binder, next-member payload, and exactly 512 correspondence visits, and fails under both a one-member source-pair rotation and a wrong-member binder environment. The reproducible ignored measurement uses one deterministic 5,000-member package and five lowering iterations. Four counterbalanced samples per arm, after one discarded warmup, compare the direct pairing with the former per-newtype name scan: median wall falls from 3.635 to 1.595 seconds (56.121%), median user-plus-system CPU from 3.685 to 1.645 seconds (55.360%), median peak RSS from 106,392 to 99,222 KiB (6.739%), and correspondence visits from 62,512,500 to 25,000 (2,500.5× fewer). All eight samples exit zero and produce the identical 5,491,996-byte Routed rendering with FNV-1a `f7c6fac31253aede`. The retained measurement summary has SHA-256 `5a5fd933bd96420d0af29a015b59494850a7deaf1d302f48329bac544a512d67`; no index, cache, serialized state, or broad-gate credit is claimed.

## Exact nominal-scope/provider acceleration and persistent alias contexts

Source: `kio-rs/src/pass/resolve.rs`, `kio-rs/src/pass/typecheck_core/{aliases,persistent_exact}.rs`.

Recorded result (deterministic semantic-operation counts and layout, not wall time): `top_level_scope_common_header_is_one_word_over_declarations` bounds the common scope header to the declaration-map header plus one word; `top_level_scope_uses_optional_shared_type_import_storage` records no index for empty or value-only imports and one shared index across clones; and `selective_type_index_shares_one_deep_provider_prefix_across_fanout` retains exactly 64 prefix segments for a 64-segment, 64-name import. `direct_nominal_lookup_uses_top_level_scope_without_declaration_scan` records zero re-export work across 128 modules. For a 64-leaf alias output, `canonical_alias_output_uses_package_scope_without_rescanning_owner_declarations` records zero declaration-item and use-edge scans despite 64 unrelated declarations and 64 unrelated imports, while `one_alias_output_reuses_its_exact_owner_entry_handle` permits at most two exact-entry lookups. The 32-deep qualified and selective fanout controls each permit at most one exact-target lookup, one indexed-prefix clone, and 32 cloned path segments. The 64-leaf canonical alias-free composite records zero materializer roots and zero argument-rope walks; the 512-binder frontier records zero ambient-binder copies. The depth-128 capture-avoidance control records one formal-summary build, at most 256 alpha-candidate probes, and at most 768 materialization calls; the rope and forwarded-owner controls bound their counters linearly in the declared depth, arity, and width while preserving the exact materialized type. No valid timed matched A/B result is available; this discharge makes no wall-time, CPU, RSS, allocation-rate, or performance-neutrality claim.

## Shared annotation/header planning and persistent lexical view

Source: `kio-rs/src/pass/typecheck_core/{annotation_plan,persistent_exact,kind_scheme,apply}.rs`, `kio-rs/src/prime/typer.rs`.

Recorded result (deterministic semantic-operation counts and firing, not wall time): `borrowed_view_is_allocation_free_and_finds_the_innermost_shadow` records zero promotions, binder scans, and persistent roots for one borrowed lookup. With depth 128 and 128 sibling views, `one_promotion_is_shared_by_all_sibling_views` records exactly one promotion, 128 scanned bindings, one persistent root, and 128 lookups. `closed_header_kind_validation_reuses_borrowed_signature_binders` records one classification with zero transport materializations, rejections, committed materializations, goals, binder publications, lexical promotions, scans, or persistent roots; `ordinary_materialization_policy_has_zero_transport_storage` pins the ordinary transport sink, node, and child storage to zero size. At depth 64, `nested_target_spelling_alpha_allocation_is_linear` records exactly 128 fresh-name probes and one visit per protected-name input node; at binder depth 32 and value width 64, `wide_expected_group_installs_one_shared_alpha_prefix` records exactly 32 alpha-edge installations. The deep and wide expected-shape controls each canonicalize once and bound combined input work to eight times the respective depth or width. Lowered check/synthesis, ordinary retained/expected-lambda, and Prime check/synthesis/immediate controls each activate exactly their named policy once. The immediate-decline control records one classification and one transport materialization, zero committed materializations, goals, or binder publications, and unchanged planning state when the exact plan transfers to the computed route. No valid timed matched A/B result is available; this discharge makes no wall-time, CPU, RSS, allocation-rate, or performance-neutrality claim.

## Empty-hole completed-argument equivalence

Source: `kio-rs/src/pass/typecheck_core/apply.rs`, exercised through `kio-rs/src/prime/typer.rs`.

Recorded result: on a fresh exact `dyn_load_prime` POC workdir with semantic caches disabled and one Rayon thread, the dev-profile compiler fell from 150.18 to 138.96 seconds wall (7.47%) and from 149,220.618 to 137,842.226 milliseconds frontend typecheck (7.63%); peak RSS changed from 613,236 to 615,484 KiB. Both arms exited zero with identical deterministic frontend workload counts. The earlier matched internal phase probe independently found all 109,352 explicit Prime calls and 150,809 finalized arguments in this workload had empty hole state and spent 25,862.136 milliseconds in completed-type equality. `empty_application_holes_use_direct_completed_type_equivalence` causally pins firing, identity and transparent-alias success, and scalar/nested mismatch diagnostics; restoring the old unconditional solver makes its firing assertion fail. `application_holes_keep_completed_type_solver` pins the nonempty-state no-op boundary and learned solution. The two `explicit_prime_argument_*` tests pin path, alias, literal, and checked-lambda success plus independently synthesized and dependent mismatch spans/messages. This focused measurement gives no broad-gate credit.

## Empty-substitution semantic-call handle reuse

Source: `kio-rs/src/pass/typecheck_core/apply.rs`.

Recorded result: on exact base `eccbf9c5307220c27e072b5daefbf2636ce5b30e`, the dev-profile control and candidate ran fresh writable copies of Git tree `26e15e63932c55e9cda69e17ba74218f3f16b6dd` with semantic caches disabled, one Rayon thread, and two warmups followed by counterbalanced `ABBA` measurement. Median wall fell from 110.585 to 108.535 seconds, a 2.050-second (1.854%) reduction; both adjacent candidate/control savings were positive at 2.300 and 1.800 seconds. Median frontend typecheck fell from 109,376.2580 to 107,502.2585 milliseconds, a 1,873.9995-millisecond (1.713%) reduction; paired savings were 2,293.755 and 1,454.244 milliseconds. Median user CPU fell by 2.000 seconds, while median peak RSS rose by 156 KiB (0.025%). All four arms exited zero with empty stdout, identical deterministic frontend workload counts, unchanged OOM-kill count, and the frozen memory, swap, and disk floors satisfied. `semantic_call_empty_substitution_reuses_only_unchanged_provenance` pins all four `(source identity, enclosing identity)` cells at the production `select` seam: canonical solver/result and noncanonical written handles share only when the output bit is unchanged, while a planned-layer downgrade produces false identity without requiring a distinct structural `Arc`. Removing the fast path made this exact test fail on its pointer-reuse assertion before restoration. `semantic_call_cursor_substitutes_width_and_advances_one_successor` pins the nonempty-substitution path through its changed selected width. This focused measurement gives no broad-gate credit.

## Goal-free retained-publication validation proof

Source: `kio-rs/src/pass/typecheck_core/goals.rs`, transported by `kio-rs/src/pass/typecheck_full/publication.rs`.

Recorded result: on the same fresh exact `dyn_load_prime` POC input used by the [completed-argument measurement](#empty-hole-completed-argument-equivalence), with semantic caches disabled, one Rayon thread, and the dev profile, the control compiler took 138.96 seconds wall / 137,842.226 milliseconds frontend typecheck / 615,484 KiB peak RSS; the retained-publication proof took 114.71 seconds / 113,793.844 milliseconds / 614,940 KiB. That is 24.25 seconds (17.4511%) less wall time and 24,048.382 milliseconds (17.4463%) less frontend typecheck time, with 544 KiB lower peak RSS; both arms exited zero on byte-identical input and the candidate produced empty stdout. The preceding attribution measured 146,723 unique payloads, 1,556,645 close occurrences, and 1,401,605 already-goal-free retained occurrences. Restoring unconditional validation makes `deep_publication_closes_payloads_without_rewalking_command_shape` fail with 2,080 full validations and zero proof reuses instead of 64 and 2,016. `prepared_publication_is_inert_until_atomic_commit` pins the first-close no-op; `goal_bearing_retained_publication_validates_until_solution_then_reuses_proof` pins full validation through the solving close and reuse only afterward; `noncanonical_retained_publication_validates_until_canonical_proof_is_minted` and `goal_free_retained_publication_rejects_an_opposite_root_kind_before_reuse` are mutation-live for the two proof-mint guards. The function-scheme alias, child-rigid, wrong-owner/store, stale-prepared, non-function, and failure/atomicity publication tests pin canonicality, exact authority, diagnostics, and mutation boundaries. This focused measurement gives no broad-gate credit.

## Deferred-tail classifier arena and scoped aliases

Source: `kio-rs/src/pass/typecheck_core/apply/fills.rs`.

Recorded result (deterministic semantic-operation counts and firing, not wall time): `deferred_tail_classifier_work_is_linear_in_alias_depth_and_scope_width` builds a 12-deep doubling pair-alias chain and a 64-wide alias environment with both `either` branch binders plus 64 nested function scopes. The former owned-tree representation performs 24,547 symbol-clone visits and 4,095 pair-mark visits on the pair fixture; whole-map scope snapshots copy 4,290 alias entries on the scope fixture. The arena representation records zero owned-symbol clones, zero alias snapshots, exactly 12 pair allocations and 12 Escape-role pair marks, a linear recipe walk, and at most 264 authored binding updates while preserving terminal-versus-escape classification through nested shadow restoration. No wall-time, CPU, RSS, allocation-rate, or end-to-end performance claim is made.

## Streaming and reusable dynamic-loader declaration scopes

Source: `test-data/poc/dyn_load_prime/workdir/loader.kio`.

The streaming-prefix step was measured on a frozen 215-declaration image: matched `ABBA` medians fell from 133.77 to 131.87 CPU-seconds and from 133.80 to 131.88 seconds wall (1.42% and 1.43%), while removing 46,225 source-level list visits; its tiny-image control showed no material change. The later reusable-scope step was remeasured on exact base `8aefebce8b163f8c524a34e6927c09eae24b9c1f` with one frozen `exec_show_cross_package_host_type` Kio' image, one compiler-built pair of driver artifacts, one runner binary, and fresh serial runner processes in candidate/control/control/candidate order. Controls took 205.57 and 205.95 seconds wall; candidates took 200.43 and 202.47, so medians fell from 205.76 to 201.45 seconds, saving 4.31 seconds (2.094%). Corresponding user-plus-system medians fell from 205.725 to 201.430 seconds; peak RSS was 33,388--33,588 KiB. All four arms exited zero, reported zero swaps, emitted identical stdout SHA-256 `bb1b98251cee0a50b9b71c545964dbc1a51517610e5b161e465bff2dcbc2146d`, and had empty stderr. The frozen image-manifest SHA-256 is `9d3c6afacb84f8cf5c89af8ed85cfc28e2ecbf814949fa791c773ba829c7586f`; the retained measurement summary SHA-256 is `f5fa09a89c3c45a9de2c6e55d8e37264a3d420e0d1cb732bb0eeb5d550acb25d`. These focused measurements give no broad-gate credit. The natural 215-declaration workload fires same-module reuse; the POC's existing single loaded image now places required `add_i32` between private `scope_before` and `scope_after`, pinning the invalidation fallback without changing the public output or one-load contract. Its private architecture guard pins the reuse and fallback wiring against becoming dead scaffolding.

## LSP signature-help presentation cost

The checker publishes already-selected grouping through existing atomic
publication, without another typecheck. Editor-only, type-free records are
bounded by checked occurrences, layers and arguments—not unique source calls.
The `signature_call_facts_are_editor_only_and_bounded_by_source_calls`
16/128-call controls retain exactly [1,N,N,N,2N] modules/calls/occurrences/
layers/arguments and visit one selected occurrence; disabled collection retains
no map. Failed-root, copied-source/merge and wide-Unicode-label controls cover
publication and one-pass offset work. Inline fact/command sizes are 32/584
bytes, not heap usage; disabled collection still has an optional inline field.
Baseline enum size was not measured.

Linux, Rust 1.99.0 dev binaries with package opt-level 2, and Node 24.21.0
measured one 128-call package. Fresh-process order was before/after/after/
before/before/after, then candidate legacy labels. Successful versioned
diagnostics preceded eight warmups and 64 complete signatureHelp requests.
Candidate responses asserted two parameters, active index 1 and the exact
selected label. All seven sessions passed; each performed one successful
analysis and 72 error-free requests, without query-triggered reanalysis.

| Arm | Three request medians (ms) | Analysis median (ms) | Peak RSS range (KiB) |
| --- | --- | --- | --- |
| Before, offsets | 1.286 / 1.247 / 1.213 | 18.630 | 29,936–30,368 |
| After, offsets | 1.409 / 1.438 / 1.343 | 18.389 | 28,148–28,448 |
| After, legacy (one session) | 1.345 | 18.341 | 28,252 |

Median-of-session request medians increased 0.161 ms (~12.9%): small added
correctness cost, not latency neutrality. Analysis ranges overlap; lower
process-peak RSS does not establish metadata-memory savings. OS caches were
not reset. These samples establish no large-workspace, edit-churn,
other-platform, allocation-rate or whole-compiler guarantee; tests use no
timing thresholds.

Compiler SHA-256: before
`bd3d37a4093a769480cde7c9822891ce909544a68f6998b8344427b4b90f8c4c`;
after `3b661df3c5ac3923860a0b5021d942d85bbfa6120eba911adcb8393124140adf`.
