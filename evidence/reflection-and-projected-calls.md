# Reflection and projected-call regression evidence

These checks cover shared frontend and compile-time evaluation behavior. They
supply no host-runtime, full-unit-suite, or complete CI pass.

A temporary variant removed four corrections together while preserving the
witness assertions. The tested paths are independent: specialization tests
invoke the reflected primitive directly; caller export constructs a pair in a
fills implementation; neutral-fuel tests invoke structural measurement; the
returned-block public case uses a non-fills identity implementation. Exact
source bytes and modes were restored before formatting. Formatting those six
fixed files reproduces their recorded restored-build hashes. The complete
build inventories also contain other changes, so this is not a claim that the
whole trees differ only in the four corrections.

| Correction | Failure with the correction removed | Restored result |
|---|---|---|
| Specialization scope | The independent `projected_specialization_rejects_unassociated_parameter_spelling_without_panicking` and `projected_specialization_reports_unused_parameter_without_panicking` tests both reach the missing-leading-binder invariant. | Both pass with the existing NoMatch and diagnostic-sum assertions. |
| Caller type export | Three unit witnesses reject caller types as fresh aliases. [`test_fills_caller_type_export`](../test-data/goldens/00_success/test_fills_caller_type_export/) exits 14 because `A & A` is compared with `A_n2 & A_n2`. | All three units pass; its public equiv passes 1/1. |
| Neutral structural fuel | `supplied_neutral_type_has_only_a_fresh_entry_lower_bound` observes an exact recursive measure of Some(1) instead of None. `neutral_head_discards_arguments_only_for_the_initial_bound` observes Some(4) instead of the initial lower bound Some(1). | Both exact unit tests pass. |
| Returned macro block | The projection unit and [`typecheck_returned_macro_block`](../test-data/goldens/00_success/typecheck_returned_macro_block/) reach the prefix-and-block-product invariant after selecting the wrong action-local stage. | The unit and public check pass. |

The returned-block negative is a reviewed **semantic reconstruction**: the
historical faulty patch bytes are unavailable. It restores the erroneous
stage-selection lifetime while preserving the exact structural validator; it
is not a byte-exact historical rollback.

[`test_equiv_nested_neutral_fuel`](../test-data/goldens/00_success/test_equiv_nested_neutral_fuel/)
passes its check and all three equivs in both variants. It supplies compatibility
coverage, **not causal failure evidence** for the neutral-fuel adapter.

Seven unchanged unit controls pass in both variants: caller/owner alias
identity, forall shadowing, repeated higher-kinded formals, deep virtual alias
measurement, authenticated projected fuel, callback root/current guards, and
absorbing faults after discarded results. Every unit invocation selected
exactly one test. Public source/package bytes match between variants, and both
arms use `kio --no-cache check` or `kio --no-cache test` to execute the subject.
The restored probes comprise eight repair-witness units, seven control units,
four CLI commands and one test-listing command; all 20 commands exit zero.

Artifact SHA-256 identities:

| Artifact | Negative | Restored |
|---|---|---|
| Unit binary | `c4c3292da2ead8c979457cb47939e57b1e56de5b0ddc08ddf168721665ae4cb5` | `0b701d956f344a21a8dd5eee1659895611b523824782d543edffaf6aae0567a3` |
| CLI | `47771e40a98f0bfc9fe33dfe300f79de152ff1cc0e00539c81a6f97c242bdffb` | `955bcf02d0f5bc91c9c17a1ac33ee8b6faa41cc6562af7b3eaa398bcab1942c9` |
| Results receipt | `fad8150def5cf6ad31dcab68da0a36ef18f5ee19e5d4bc5de320de6c33795b35` | `2031bbe159f63a426bd2558a332324a414a922fc33ff6a2075ff021a453eb7ed` |
