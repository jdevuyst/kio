//! Self-test layer (2): distribution / corpus assertions.
//!
//! These tests don't check individual programs (that's the layer
//! (1) job in `selfcheck.rs`); they assert the corpus as a whole is
//! non-degenerate and that the mutator catalog matches the
//! exit-code coverage discipline declared in the working plan.
//!
//! The spec-coverage test is the load-bearing one: when a new code
//! lands in `specs/exit-codes.md`, this test fails until either a
//! matching mutator is added or the new code is explicitly marked
//! non-coverable in the design notes.
//!
//! Thresholds are deliberately loose: layer (2) is a degeneracy
//! check, not a tuning knob. False-positives are worse than missing
//! a subtle bias here.

use std::collections::HashSet;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use kio_gen::ast::{Base, Term, Type, UserElaboratorImplementation, UserElaboratorTemplate};
use kio_gen::{generate, mutate, program_seed, render};

const N: u64 = 2000;
const RUN_SEED: u64 = 0xD15D_D15D_D15D_D15D;

#[test]
fn corpus_is_non_degenerate() {
    let progs: Vec<_> = (0..N)
        .map(|i| generate::program_from_seed(program_seed(RUN_SEED, i)))
        .collect();

    // Hash collisions: rendered programs should be near-unique.
    let unique: HashSet<_> = progs.iter().map(render::render_fn_def).collect();
    let dup_rate = (N as usize - unique.len()) as f64 / N as f64;
    // 25% is a true-degeneracy threshold: small bodies and a finite
    // base-type pool guarantee some collisions; >25% would mean the
    // generator is genuinely stuck on a small set of outputs.
    assert!(
        dup_rate < 0.25,
        "rendered duplicate rate {dup_rate:.2} above 25%"
    );

    // Depth: the body must average non-trivial depth.
    let avg_depth: f64 = progs
        .iter()
        .map(|p| term_depth(&p.body) as f64)
        .sum::<f64>()
        / N as f64;
    assert!(avg_depth >= 2.0, "avg body depth {avg_depth} too shallow");

    // Every base type appears at least once across the corpus.
    // The base pool now spans all 12 numeric kinds plus Str/Bool, so
    // a 10% per-base floor would be impossibly tight; "every base
    // shows up" is enough to catch a degenerate picker.
    let mut seen_bases: HashSet<Base> = HashSet::new();
    for p in &progs {
        if let Type::Base(b) = p.ret {
            seen_bases.insert(b);
        }
    }
    for b in Base::all() {
        assert!(
            seen_bases.contains(b),
            "base {b:?} never appears as a return type across {N} programs"
        );
    }

    // Polymorphism + the if-then-else intrinsic must each show up at
    // least once across the corpus — strong evidence the generator
    // isn't stuck on its other productions.
    let with_id = progs
        .iter()
        .filter(|p| contains_poly_call(&p.body, "id"))
        .count();
    assert!(with_id >= 1, "no `id` poly-call across {N} programs");

    let with_ite = progs
        .iter()
        .filter(|p| contains_poly_call(&p.body, "__if_then_else__"))
        .count();
    assert!(with_ite >= 1, "no `__if_then_else__` across {N} programs");
}

#[test]
fn coverable_exit_codes_match_spec_and_mutator_catalog() {
    // Stage-2 coverable codes (post slice 2.11): 0 (unmutated path),
    // 11/12/13/14/15 (covered by the mutator catalog). Codes 1, 2,
    // 10, 16, 20, 40 are deliberately outside the catalog (see
    // "Exit-code coverage" in ci/infra/kio-gen-rs/README.md for the
    // reasoning — 16 is the totality bucket and is a candidate for
    // a future mutator).
    let mut codes: Vec<u32> = mutate::Mutation::all()
        .iter()
        .map(|m| m.exit_code())
        .collect();
    codes.sort();
    assert_eq!(
        codes,
        vec![11, 12, 13, 14, 15],
        "stage-2 mutator catalog must exactly match coverable invalid codes \
         per specs/exit-codes.md"
    );
}

#[test]
fn surface_forms_appear_in_corpus() {
    // Cross-section signal: at least one program's rendered fn
    // exercises a surface form (tuple sugar, `if`/`else`, etc.). A
    // surface-using program has `uses_surface == true`; the more
    // granular per-form checks below pin "this *specific* surface
    // form fires at least once" — they fail loudly when a production
    // gets accidentally weighted out of existence.
    let progs: Vec<_> = (0..N)
        .map(|i| generate::program_from_seed(program_seed(RUN_SEED, i)))
        .collect();
    let surface_using = progs.iter().filter(|p| p.uses_surface).count();
    assert!(
        surface_using > 0,
        "no program flagged as surface-using across {N} seeds"
    );

    // Per-surface-form rendered-text checks: scan the rendered fn
    // for a string that only the surface render path produces.
    let with_tuple_sugar = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_tuple_sugar(&render::render_fn_def(p)))
        .count();
    let with_if_else = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_if_else(&render::render_fn_def(p)))
        .count();
    let with_elaborator = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_elaborator(&render::render_fn_def(p)))
        .count();
    // Per-form elaborator counts. The above bundles into! + onto!
    // together; splitting them surfaces an emission asymmetry early
    // (one form going to zero would otherwise hide behind the union).
    let with_into = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_into(&render::render_fn_def(p)))
        .count();
    let with_onto = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_onto(&render::render_fn_def(p)))
        .count();
    let with_iso = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_iso(&render::render_fn_def(p)))
        .count();
    let with_align = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_align(&render::render_fn_def(p)))
        .count();
    let with_ease = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_ease(&render::render_fn_def(p)))
        .count();
    let with_atom = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_atom(&render::render_fn_def(p)))
        .count();
    // Per-spine-form counts. The spine palette is the everyday
    // material and should dominate the wrap choice; `fit!` is the
    // most general form and gets a higher per-form weight. Each form
    // has its own count to surface an emission asymmetry early.
    let with_reorder_sum = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_reorder_sum(&render::render_fn_def(p)))
        .count();
    let with_reorder_prod = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_reorder_prod(&render::render_fn_def(p)))
        .count();
    let with_narrow_sum = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_narrow_sum(&render::render_fn_def(p)))
        .count();
    let with_narrow_prod = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_narrow_prod(&render::render_fn_def(p)))
        .count();
    let with_widen_sum = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_widen_sum(&render::render_fn_def(p)))
        .count();
    let with_widen_prod = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_widen_prod(&render::render_fn_def(p)))
        .count();
    let with_flatten_sum = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_flatten_sum(&render::render_fn_def(p)))
        .count();
    let with_flatten_prod = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_flatten_prod(&render::render_fn_def(p)))
        .count();
    let with_one_sum = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_one_sum(&render::render_fn_def(p)))
        .count();
    // `one_prod!` is currently restricted to atomic sources (the
    // degenerate-identity case). Like `one_sum!`, this exercises
    // the surface-form codepath without committing to the wider
    // applicability cases the spec admits.
    let with_one_prod = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_one_prod(&render::render_fn_def(p)))
        .count();
    let with_fit = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_fit(&render::render_fn_def(p)))
        .count();
    let with_ufcs = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_ufcs(&render::render_fn_def(p)))
        .count();
    let with_alias_lit = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_alias_lit(&render::render_fn_def(p)))
        .count();
    let with_op_call = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_op_call(&render::render_fn_def(p)))
        .count();
    let with_fn_placeholder = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_fn_placeholder(&render::render_fn_def(p)))
        .count();
    let with_user_elaborator_call = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_user_elaborator_call(&render::render_fn_def(p)))
        .count();
    let with_user_elaborator_def = progs
        .iter()
        .filter(|p| p.surface_mode && !p.user_elaborators.is_empty())
        .count();
    let mut user_elaborator_templates: HashSet<UserElaboratorTemplate> = HashSet::new();
    let mut user_elaborator_capture_shapes: HashSet<Vec<String>> = HashSet::new();
    let mut fills_implementations = 0;
    let mut user_elaborator_body_calls = 0;
    for p in &progs {
        let fills_in_program = p
            .user_elaborators
            .iter()
            .filter(|spec| {
                matches!(
                    spec.implementation,
                    UserElaboratorImplementation::FillsIdentity
                )
            })
            .count();
        assert_eq!(
            fills_in_program,
            usize::from(p.surface_mode),
            "every surface program must carry exactly one fixed fills witness"
        );
        let mut body_call_names = Vec::new();
        collect_user_elaborator_call_names(&p.body, &mut body_call_names);
        user_elaborator_body_calls += body_call_names.len();
        for name in body_call_names {
            let spec = p
                .user_elaborators
                .iter()
                .find(|spec| spec.name == name)
                .unwrap_or_else(|| panic!("body calls undeclared user elaborator `{name}`"));
            assert!(
                matches!(spec.implementation, UserElaboratorImplementation::Late(_)),
                "random body wrapper called fixed fills elaborator `{name}`"
            );
        }
        for spec in &p.user_elaborators {
            match spec.implementation {
                UserElaboratorImplementation::Late(template) => {
                    user_elaborator_templates.insert(template);
                    user_elaborator_capture_shapes.insert(
                        spec.captures
                            .iter()
                            .map(|capture| format!("{capture:?}"))
                            .collect(),
                    );
                }
                UserElaboratorImplementation::FillsIdentity => {
                    fills_implementations += 1;
                    assert!(
                        spec.captures.is_empty(),
                        "the generated fills identity must remain capture-free"
                    );
                }
            }
        }
    }
    // `equiv` lives at item position, not inside the fn body — so
    // we read the full case files (the equiv block is appended in
    // `emit::render_package_files`). We assert presence in the
    // emitted package text for any `surface_mode` program (the
    // gating flag in `emit.rs`).
    let with_equiv = progs.iter().filter(|p| p.surface_mode).count();
    let with_label_value = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_label_value(&render::render_fn_def(p)))
        .count();
    let with_match_bang = progs
        .iter()
        .filter(|p| p.uses_surface && body_has_match_bang(&render::render_fn_def(p)))
        .count();
    let with_reserved_id = progs
        .iter()
        .filter(|p| body_has_reserved_id(&render::render_fn_def(p)))
        .count();
    assert!(
        with_tuple_sugar > 0,
        "no program rendered with tuple-literal sugar across {N} seeds"
    );
    assert!(
        with_if_else > 0,
        "no program rendered with surface `if`/`else` across {N} seeds"
    );
    assert!(
        with_elaborator > 0,
        "no program rendered with `into!` / `onto!` across {N} seeds"
    );
    assert!(
        with_into > 0,
        "no program rendered with `into!` across {N} seeds — \
         either `into!` is being weighted out, or `body_has_into` \
         needs updating"
    );
    assert!(
        with_onto > 0,
        "no program rendered with `onto!` across {N} seeds — \
         either `onto!` is being weighted out, or `body_has_onto` \
         needs updating"
    );
    assert!(
        with_iso > 0,
        "no program rendered with `iso!` across {N} seeds — \
         either `iso!` is being weighted out, or `body_has_iso` \
         needs updating"
    );
    assert!(
        with_align > 0,
        "no program rendered with `align!` across {N} seeds — \
         either `align!` is being weighted out, or `body_has_align` \
         needs updating"
    );
    assert!(
        with_ease > 0,
        "no program rendered with `ease!` across {N} seeds — \
         either `ease!` is being weighted out, or `body_has_ease` \
         needs updating"
    );
    assert!(
        with_atom > 0,
        "no program rendered with `atom!` across {N} seeds — \
         either `atom!` is being weighted out, or `body_has_atom` \
         needs updating"
    );
    // Spine-palette per-form asserts. `fit!` admits identity at
    // every type and carries the highest per-form weight, so it
    // should reliably appear in any healthy {N}-program corpus.
    // The reorder / narrow / widen / flatten variants need their
    // axis shape (sum / product) to come up; axis-shaped check
    // positions are scarce enough that the corpus size N is chosen
    // to give every axis-gated form a comfortable floor margin.
    // `one_sum!` / `one_prod!` need an atomic source AND the
    // algebraic-vs-spine subset roll to land on spine AND the
    // per-form weight to hit; floor is 1.
    assert!(
        with_reorder_sum > 0,
        "no program rendered with `reorder_sum!` across {N} seeds — \
         either `reorder_sum!` is being weighted out, or \
         `body_has_reorder_sum` needs updating"
    );
    assert!(
        with_reorder_prod > 0,
        "no program rendered with `reorder_prod!` across {N} seeds — \
         either `reorder_prod!` is being weighted out, or \
         `body_has_reorder_prod` needs updating"
    );
    assert!(
        with_narrow_sum > 0,
        "no program rendered with `narrow_sum!` across {N} seeds — \
         either `narrow_sum!` is being weighted out, or \
         `body_has_narrow_sum` needs updating"
    );
    assert!(
        with_narrow_prod > 0,
        "no program rendered with `narrow_prod!` across {N} seeds — \
         either `narrow_prod!` is being weighted out, or \
         `body_has_narrow_prod` needs updating"
    );
    assert!(
        with_widen_sum > 0,
        "no program rendered with `widen_sum!` across {N} seeds — \
         either `widen_sum!` is being weighted out, or \
         `body_has_widen_sum` needs updating"
    );
    assert!(
        with_widen_prod > 0,
        "no program rendered with `widen_prod!` across {N} seeds — \
         either `widen_prod!` is being weighted out, or \
         `body_has_widen_prod` needs updating"
    );
    assert!(
        with_flatten_sum > 0,
        "no program rendered with `flatten_sum!` across {N} seeds — \
         either `flatten_sum!` is being weighted out, or \
         `body_has_flatten_sum` needs updating"
    );
    assert!(
        with_flatten_prod > 0,
        "no program rendered with `flatten_prod!` across {N} seeds — \
         either `flatten_prod!` is being weighted out, or \
         `body_has_flatten_prod` needs updating"
    );
    assert!(
        with_one_sum > 0,
        "no program rendered with `one_sum!` across {N} seeds — \
         either `one_sum!` is being weighted out, or \
         `body_has_one_sum` needs updating"
    );
    assert!(
        with_one_prod > 0,
        "no program rendered with `one_prod!` across {N} seeds — \
         either `one_prod!` is being weighted out, or \
         `body_has_one_prod` needs updating"
    );
    assert!(
        with_fit > 0,
        "no program rendered with `fit!` across {N} seeds — \
         either `fit!` is being weighted out, or `body_has_fit` \
         needs updating"
    );
    // Spine should dominate algebraic. The wrap subset roll is
    // 80/20 spine/algebraic; the spine_count / algebraic_count
    // ratio across the corpus should reflect that bias. Floor: the
    // total spine count exceeds the total algebraic count, which
    // would fail loudly if the per-subset roll got flipped.
    let spine_total = with_reorder_sum
        + with_reorder_prod
        + with_narrow_sum
        + with_narrow_prod
        + with_widen_sum
        + with_widen_prod
        + with_flatten_sum
        + with_flatten_prod
        + with_one_sum
        + with_one_prod
        + with_fit;
    let algebraic_total = with_into + with_onto + with_iso + with_align + with_ease + with_atom;
    assert!(
        spine_total > algebraic_total,
        "spine palette ({spine_total}) should dominate algebraic ({algebraic_total}) — \
         the wrap-subset roll is 80/20 spine/algebraic; if this assertion fails the \
         split is reversed or the spine-form per-form weights are wrong"
    );
    assert!(
        with_ufcs > 0,
        "no program rendered with UFCS (`.>`) across {N} seeds"
    );
    assert!(
        with_alias_lit > 0,
        "no program rendered with a literal alias reference (`zero_i32` / `hello_str`) \
         across {N} seeds"
    );
    assert!(
        with_op_call > 0,
        "no program rendered with a user-op chain use (`<+>`) across {N} seeds"
    );
    assert!(
        with_fn_placeholder > 0,
        "no program rendered with a `.arg.` placeholder lambda across {N} seeds"
    );
    assert!(
        with_user_elaborator_def > 0,
        "no surface-mode program emitted generated `elab` definitions across {N} seeds"
    );
    assert!(
        with_user_elaborator_call > 0,
        "no program rendered with a generated user elaborator call across {N} seeds"
    );
    assert!(
        user_elaborator_body_calls > 0,
        "no generated body contains a structurally recorded user elaborator call across {N} seeds"
    );
    for template in UserElaboratorTemplate::all() {
        assert!(
            user_elaborator_templates.contains(template),
            "generated user elaborator template {template:?} never appeared across {N} seeds"
        );
    }
    assert!(
        fills_implementations > 0,
        "no `impl(fills)` user elaborator appeared across {N} seeds"
    );
    assert!(
        user_elaborator_capture_shapes.len() >= 2,
        "generated user elaborator captures did not vary across {N} seeds"
    );
    // Check the generated root modules are written out with
    // the current elaborator ABI, not just represented in Program metadata.
    let mut generated_elaborator_modules = 0;
    let mut proof_threaded_helper_seen = false;
    for p in progs
        .iter()
        .filter(|p| p.surface_mode && !p.user_elaborators.is_empty())
    {
        let pkg = kio_gen::emit::render_package_files(p);
        let generated = pkg
            .iter()
            .find(|(n, _)| n.ends_with("kio_gen_elaborators.kio"))
            .map(|(_, s)| s.as_str())
            .unwrap_or("");
        let root = pkg
            .iter()
            .find(|(n, _)| n == "workdir/prog.kio")
            .map(|(_, s)| s.as_str())
            .unwrap_or("");
        generated_elaborator_modules += 1;
        assert!(
            generated.contains("pub elab kio_gen_elab0"),
            "surface-mode package missing generated `pub elab` declarations"
        );
        for spec in &p.user_elaborators {
            let fn_needle = format!("pure fn {}(ct: __Comptime__", spec.impl_name);
            let public_fn_needle = format!("pub {fn_needle}");
            assert!(
                generated.contains(&fn_needle),
                "generated implementation {} is not private or does not bind `ct: __Comptime__` first",
                spec.impl_name
            );
            assert!(
                !generated.contains(&public_fn_needle),
                "generated implementation {} is public",
                spec.impl_name
            );
            match spec.implementation {
                UserElaboratorImplementation::Late(_) => {
                    let declaration = format!("impl {} }}\n", spec.impl_name);
                    assert!(
                        generated.contains(&declaration),
                        "late implementation {} lost its ordinary declaration",
                        spec.impl_name
                    );
                }
                UserElaboratorImplementation::FillsIdentity => {
                    let declaration = format!("impl(fills) {} }}\n", spec.impl_name);
                    let signature = format!(
                        "pure fn {}(ct: __Comptime__, fills: __Fill_ctx__, source: __Type__, value: __Checked_term__, target: __Type__) -> __Checked_term__ & __Fill_ctx__",
                        spec.impl_name
                    );
                    assert!(generated.contains(&declaration));
                    assert!(generated.contains(&signature));
                    assert!(generated.contains("(value, __fill__(ct, fills, target, source))"));
                    let witness = format!(
                        "fn kio_gen_fills_witness(value: I32) -> I32 {{ {}!(value, _) }}",
                        spec.name
                    );
                    assert!(
                        root.contains(&witness),
                        "the fixed witness must call the selected fills elaborator"
                    );
                }
            }
        }
        proof_threaded_helper_seen |= generated.contains("__type_equal__(ct")
            || generated.contains("__term_type__(ct")
            || generated.contains("__type_error__(ct")
            || generated.contains("__term_let__(\n  , ct")
            || generated.contains("__type_product__(ct")
            || generated.contains("__type_sum__(ct");
        assert!(
            !generated.contains("__type_equal__(source")
                && !generated.contains("__type_equal__(__term_type__")
                && !generated.contains("__term_type__(value")
                && !generated.contains("__type_error__(\"")
                && !generated.contains("__term_let__(\n  , source")
                && !generated.contains("__type_product__(source")
                && !generated.contains("__type_sum__(source"),
            "generated user elaborator module contains a stale no-proof helper call"
        );
    }
    assert!(
        generated_elaborator_modules > 0,
        "surface-mode package missing generated elaborator module output"
    );
    assert!(
        proof_threaded_helper_seen,
        "generated user elaborator helpers are not proof-threaded"
    );
    assert!(
        with_equiv > 0,
        "no surface-mode program seen across {N} seeds — `equiv` blocks ride on \
         `surface_mode` in emit.rs"
    );
    // Spot-check the equiv block is actually written out by rendering
    // one surface_mode program to its package files.
    if let Some(p) = progs.iter().find(|p| p.surface_mode) {
        let pkg = kio_gen::emit::render_package_files(p);
        let root = pkg
            .iter()
            .find(|(n, _)| n == "workdir/prog.kio")
            .map(|(_, s)| s.as_str())
            .unwrap_or("");
        assert!(
            root.contains("equiv kio_gen_unit_equiv { (); id(., ()) }"),
            "surface-mode prog.kio missing the Unit type/value `equiv` witness"
        );
        assert!(
            root.contains(
                "pub pure fn kio_gen_fold_step(_value: I32, _acc: I32) -> I32 { 0(I32) }"
            ) && root.contains("pub varop [* *] { foldr1 kio_gen_fold_step kio_gen_fold_seed }\n")
                && root.contains("equiv kio_gen_seeded_fold_singleton { [* 2(I32) *]; 2(I32) }")
                && root.contains("equiv kio_gen_seeded_fold_step { [* 1(I32), 2(I32) *]; 0(I32) }"),
            "surface-mode prog.kio missing nonvacuous seeded variadic-operator coverage"
        );
        assert!(
            root.contains("labels Kio_gen_labels = { greet : _ } | { count : _ }"),
            "surface-mode prog.kio missing explicit repeated-label reuse coverage"
        );
        assert!(
            root.contains("pub type {kio_gen_count_forward} = {count};"),
            "surface-mode prog.kio missing the fixed nonminting label forward"
        );
        assert!(
            root.contains(
                "equiv kio_gen_label_forward_nominal { {kio_gen_count_forward = 7(I32)}; Count.mk(7(I32)) }"
            ),
            "surface-mode prog.kio missing forwarded-to-original nominal equivalence"
        );
        assert!(
            root.contains(
                "equiv kio_gen_label_forward_payload { Count.mk(11(I32)).?{kio_gen_count_forward}; 11(I32) }"
            ),
            "surface-mode prog.kio missing independently constructed payload-access coverage"
        );
    }
    let core = progs
        .iter()
        .find(|p| !p.surface_mode)
        .expect("the sampled corpus must include a core-mode program");
    let core_files = kio_gen::emit::render_package_files(core);
    let core_root = core_files
        .iter()
        .find(|(path, _)| path == "workdir/prog.kio")
        .map(|(_, source)| source.as_str())
        .expect("core-mode package must have a root module");
    for surface_fragment in [
        "pub type {kio_gen_count_forward}",
        "equiv kio_gen_label_forward_nominal",
        "equiv kio_gen_label_forward_payload",
    ] {
        assert!(
            !core_root.contains(surface_fragment),
            "core-mode prog.kio contains surface-only forwarding: {surface_fragment}"
        );
    }
    assert!(
        with_label_value > 0,
        "no program rendered with label-value sugar `{{label = e}}` across {N} seeds"
    );
    assert!(
        with_match_bang > 0,
        "no program rendered with `match!` across {N} seeds"
    );
    assert!(
        with_reserved_id > 0,
        "no program rendered with a reserved-elsewhere identifier across {N} seeds — \
         the JS-codegen / cross-language identifier-mangling path isn't being exercised"
    );
}

fn body_has_tuple_sugar(rendered: &str) -> bool {
    // Tuple sugar shape: `(a, b)` outside of a __pair__ call.
    // Approximate: look for `, ` that is followed by something other
    // than a type-shaped name. Easier heuristic: the
    // generator's surface render NEVER emits __pair__, so seeing
    // *any* `(<expr>, <expr>)` outside the function-signature parens
    // is a safe-enough signal. The simplest stable signal is "the
    // word __pair__ does not appear in the rendered output for
    // this program AND there's at least one comma inside parens
    // somewhere past the signature." For test purposes the absence
    // of __pair__ in a rendered fn that *should* have it is
    // already strong enough.
    !rendered.contains("__pair__") && rendered.contains(", ")
}

fn body_has_if_else(rendered: &str) -> bool {
    // The surface conditional uses the ordinary imported block elaborator;
    // Kio' mode emits `__if_then_else__` instead.
    rendered.contains("if!(") && rendered.contains("} else {")
}

fn body_has_elaborator(rendered: &str) -> bool {
    // Surface elaborator forms (`iso!`, `into!`, `onto!`, `align!`,
    // `ease!`, `atom!`) are only emitted in surface mode; Kio' mode
    // passes through the inner expression. The bang-call is the
    // unique signal.
    body_has_into(rendered)
        || body_has_onto(rendered)
        || body_has_iso(rendered)
        || body_has_align(rendered)
        || body_has_ease(rendered)
        || body_has_atom(rendered)
}

fn body_has_into(rendered: &str) -> bool {
    rendered.contains("into!(")
}

fn body_has_onto(rendered: &str) -> bool {
    rendered.contains("onto!(")
}

fn body_has_iso(rendered: &str) -> bool {
    rendered.contains("iso!(")
}

fn body_has_align(rendered: &str) -> bool {
    rendered.contains("align!(")
}

fn body_has_ease(rendered: &str) -> bool {
    rendered.contains("ease!(")
}

fn body_has_atom(rendered: &str) -> bool {
    rendered.contains("atom!(")
}

fn body_has_reorder_sum(rendered: &str) -> bool {
    rendered.contains("reorder_sum!(")
}

fn body_has_reorder_prod(rendered: &str) -> bool {
    rendered.contains("reorder_prod!(")
}

fn body_has_narrow_sum(rendered: &str) -> bool {
    rendered.contains("narrow_sum!(")
}

fn body_has_narrow_prod(rendered: &str) -> bool {
    rendered.contains("narrow_prod!(")
}

fn body_has_widen_sum(rendered: &str) -> bool {
    rendered.contains("widen_sum!(")
}

fn body_has_widen_prod(rendered: &str) -> bool {
    rendered.contains("widen_prod!(")
}

fn body_has_flatten_sum(rendered: &str) -> bool {
    rendered.contains("flatten_sum!(")
}

fn body_has_flatten_prod(rendered: &str) -> bool {
    rendered.contains("flatten_prod!(")
}

fn body_has_one_sum(rendered: &str) -> bool {
    rendered.contains("one_sum!(")
}

fn body_has_one_prod(rendered: &str) -> bool {
    rendered.contains("one_prod!(")
}

fn body_has_fit(rendered: &str) -> bool {
    rendered.contains("fit!(")
}

fn body_has_ufcs(rendered: &str) -> bool {
    rendered.contains(".>")
}

fn body_has_alias_lit(rendered: &str) -> bool {
    // Surface mode renders `Term::LiteralAliasRef { name, .. }` as an
    // annotated alias call; Kio' mode substitutes the bound literal. The two
    // alias names `zero_i32` / `hello_str` are unique to the
    // literal-alias surface path.
    rendered.contains("zero_i32") || rendered.contains("hello_str")
}

fn body_has_op_call(rendered: &str) -> bool {
    // The surface user-op `<+>` declared in prog.kio fires from
    // the OpCall production. The op-token sequence is unique
    // enough that any `<+>` outside the import-line context is a
    // use-site. `render::render_fn_def` only renders the fn body,
    // so there are no import lines in the input — any `<+>`
    // signals a use.
    rendered.contains(" <+> ")
}

fn body_has_fn_placeholder(rendered: &str) -> bool {
    // Surface `.arg. { ... }` is unique to the FnPlaceholder
    // production; the Kio' fallback renders a regular `.(...)
    // { ... }` instead.
    rendered.contains(".arg. {")
}

fn body_has_user_elaborator_call(rendered: &str) -> bool {
    rendered.contains("kio_gen_elab") && rendered.contains("!(")
}

fn body_has_label_value(rendered: &str) -> bool {
    // Surface `{<label> = …}` value-construction sugar fires only in
    // surface mode; the Kio' fallback emits `mk_<label>(…)`. The
    // `{greet = ` / `{count = ` shape is unique to the surface form.
    rendered.contains("{greet = ") || rendered.contains("{count = ")
}

fn body_has_match_bang(rendered: &str) -> bool {
    // Surface `match!(<scrutinee>) { … }` is only emitted by the
    // MatchBang surface render path; the Kio' fallback prints
    // `__either__(...)`. The `match!` token is unique to the
    // surface form.
    rendered.contains("match!(")
}

fn body_has_reserved_id(rendered: &str) -> bool {
    // Slice 2.16: the generator occasionally picks binding names
    // from a curated catalog of reserved/keyword identifiers from
    // popular languages. Surface text-search a small representative
    // subset — JS, Python, Rust, Go — to confirm at least one
    // program in the corpus lands one. The catalog is much larger;
    // any single name from each language is enough to declare
    // "reserved-id biasing fired".
    let needles = [
        "let class = ",
        "let default = ",
        "let return = ",
        "let async = ",
        "let await = ",
        "let function = ",
        "let new = ",
        "let delete = ",
        "let typeof = ",
        "let var = ",
        "let const = ",
        "let extends = ",
        "let super = ",
        "let this = ",
        "let void = ",
        "let yield = ",
        "let arguments = ",
        "let eval = ",
        "let import = ",
        "let export = ",
        "let def = ",
        "let lambda = ",
        "let pass = ",
        "let print = ",
        "let with = ",
        "let assert = ",
        "let raise = ",
        "let global = ",
        "let nonlocal = ",
        "let interface = ",
        "let implements = ",
        "let static = ",
        "let final = ",
        "let move = ",
        "let mut = ",
        "let dyn = ",
        "let impl = ",
        "let trait = ",
        "let chan = ",
        "let defer = ",
        "let go = ",
        "let func = ",
        "let struct = ",
        "let guard = ",
        "let init = ",
        ".(class)",
        ".(default)",
        ".(class,",
        ".(default,",
    ];
    needles.iter().any(|n| rendered.contains(n))
}

#[test]
fn corpus_body_shape_distribution() {
    // Both sides (body renders as Kio' vs. body uses surface forms)
    // should each be at least 20% of the corpus, so the Kio' render
    // path and the surface-form codegen-and-elaboration path each get
    // meaningful exposure on every batch. With surface_mode flipping
    // a 50/50 coin per program, a sampled 500-program batch sits
    // around 33%/67% prime/surface in practice — 20% is a generous
    // floor that catches a stuck generator without flagging healthy
    // noise.
    let progs: Vec<_> = (0..N)
        .map(|i| generate::program_from_seed(program_seed(RUN_SEED, i)))
        .collect();
    let surface = progs.iter().filter(|p| p.uses_surface).count();
    let prime = N as usize - surface;
    let floor = (N as usize) / 5; // 20%
    assert!(
        prime >= floor,
        "Kio'-only programs ({prime}) below 20% of {N}-program corpus floor ({floor})"
    );
    assert!(
        surface >= floor,
        "surface-using programs ({surface}) below 20% of {N}-program corpus floor ({floor})"
    );
}

#[test]
fn pick_exercises_all_invalid_codes_in_a_corpus() {
    // mutate::pick is uniform over the catalog; assert that in N
    // draws every invalid code is touched at least once. If a new
    // mutation is added with very low weight this would surface.
    let mut rng = ChaCha20Rng::seed_from_u64(RUN_SEED);
    let mut seen: HashSet<u32> = HashSet::new();
    for _ in 0..N {
        seen.insert(mutate::pick(&mut rng).exit_code());
    }
    for code in [11, 12, 13, 14, 15] {
        assert!(
            seen.contains(&code),
            "exit code {code} never picked in {N} tries"
        );
    }
}

fn term_depth(t: &Term) -> u32 {
    match t {
        Term::NumLit { .. } | Term::StrLit(_) | Term::BoolLit(_) | Term::UnitLit | Term::Var(_) => {
            0
        }
        Term::Lambda { body, .. } => 1 + term_depth(body),
        Term::App(f, args) => 1 + term_depth(f).max(args.iter().map(term_depth).max().unwrap_or(0)),
        Term::Let { rhs, body, .. } => 1 + term_depth(rhs).max(term_depth(body)),
        Term::PolyCall { args, .. } | Term::TypeMember { args, .. } => {
            1 + args.iter().map(term_depth).max().unwrap_or(0)
        }
        Term::IfElse {
            cond, then, else_, ..
        } => {
            1 + term_depth(cond)
                .max(term_depth(then))
                .max(term_depth(else_))
        }
        Term::LabelConstruct { payload, .. } => 1 + term_depth(payload),
        Term::Into { inner }
        | Term::Onto { inner }
        | Term::Iso { inner }
        | Term::Align { inner }
        | Term::Ease { inner }
        | Term::Atom { inner }
        | Term::ReorderSum { inner }
        | Term::ReorderProd { inner }
        | Term::NarrowSum { inner }
        | Term::NarrowProd { inner }
        | Term::WidenSum { inner }
        | Term::WidenProd { inner }
        | Term::FlattenSum { inner }
        | Term::FlattenProd { inner }
        | Term::OneSum { inner }
        | Term::OneProd { inner }
        | Term::Fit { inner } => 1 + term_depth(inner),
        Term::Ufcs {
            receiver,
            rest_args,
            ..
        } => 1 + term_depth(receiver).max(rest_args.iter().map(term_depth).max().unwrap_or(0)),
        Term::FnPlaceholder { body, .. } => 1 + term_depth(body),
        Term::Placeholder { .. } => 0,
        Term::LiteralAliasRef { .. } => 0,
        Term::OpCall { lhs, rhs, .. } => 1 + term_depth(lhs).max(term_depth(rhs)),
        Term::UserElaborator { inner, .. } => 1 + term_depth(inner),
        Term::MatchBang {
            scrutinee,
            left_body,
            right_body,
            ..
        } => {
            1 + term_depth(scrutinee)
                .max(term_depth(left_body))
                .max(term_depth(right_body))
        }
    }
}

fn collect_user_elaborator_call_names<'a>(term: &'a Term, names: &mut Vec<&'a str>) {
    match term {
        Term::NumLit { .. }
        | Term::StrLit(_)
        | Term::BoolLit(_)
        | Term::UnitLit
        | Term::Var(_)
        | Term::Placeholder { .. } => {}
        Term::Lambda { body, .. } | Term::FnPlaceholder { body, .. } => {
            collect_user_elaborator_call_names(body, names);
        }
        Term::App(callee, args) => {
            collect_user_elaborator_call_names(callee, names);
            for arg in args {
                collect_user_elaborator_call_names(arg, names);
            }
        }
        Term::Let { rhs, body, .. } => {
            collect_user_elaborator_call_names(rhs, names);
            collect_user_elaborator_call_names(body, names);
        }
        Term::PolyCall { args, .. } | Term::TypeMember { args, .. } => {
            for arg in args {
                collect_user_elaborator_call_names(arg, names);
            }
        }
        Term::IfElse {
            cond, then, else_, ..
        } => {
            collect_user_elaborator_call_names(cond, names);
            collect_user_elaborator_call_names(then, names);
            collect_user_elaborator_call_names(else_, names);
        }
        Term::LabelConstruct { payload, .. } => {
            collect_user_elaborator_call_names(payload, names);
        }
        Term::MatchBang {
            scrutinee,
            left_body,
            right_body,
            ..
        } => {
            collect_user_elaborator_call_names(scrutinee, names);
            collect_user_elaborator_call_names(left_body, names);
            collect_user_elaborator_call_names(right_body, names);
        }
        Term::Into { inner }
        | Term::Onto { inner }
        | Term::Iso { inner }
        | Term::Align { inner }
        | Term::Ease { inner }
        | Term::Atom { inner }
        | Term::ReorderSum { inner }
        | Term::ReorderProd { inner }
        | Term::NarrowSum { inner }
        | Term::NarrowProd { inner }
        | Term::WidenSum { inner }
        | Term::WidenProd { inner }
        | Term::FlattenSum { inner }
        | Term::FlattenProd { inner }
        | Term::OneSum { inner }
        | Term::OneProd { inner }
        | Term::Fit { inner }
        | Term::LiteralAliasRef { literal: inner, .. } => {
            collect_user_elaborator_call_names(inner, names);
        }
        Term::Ufcs {
            receiver,
            rest_args,
            ..
        } => {
            collect_user_elaborator_call_names(receiver, names);
            for arg in rest_args {
                collect_user_elaborator_call_names(arg, names);
            }
        }
        Term::OpCall { lhs, rhs, .. } => {
            collect_user_elaborator_call_names(lhs, names);
            collect_user_elaborator_call_names(rhs, names);
        }
        Term::UserElaborator { name, inner } => {
            names.push(name);
            collect_user_elaborator_call_names(inner, names);
        }
    }
}

fn contains_poly_call(t: &Term, name: &str) -> bool {
    match t {
        Term::NumLit { .. } | Term::StrLit(_) | Term::BoolLit(_) | Term::UnitLit | Term::Var(_) => {
            false
        }
        Term::Lambda { body, .. } => contains_poly_call(body, name),
        Term::App(f, args) => {
            contains_poly_call(f, name) || args.iter().any(|a| contains_poly_call(a, name))
        }
        Term::Let { rhs, body, .. } => {
            contains_poly_call(rhs, name) || contains_poly_call(body, name)
        }
        Term::PolyCall { name: n, args, .. } => {
            n == name || args.iter().any(|a| contains_poly_call(a, name))
        }
        Term::TypeMember { args, .. } => args.iter().any(|a| contains_poly_call(a, name)),
        Term::IfElse {
            cond, then, else_, ..
        } => {
            contains_poly_call(cond, name)
                || contains_poly_call(then, name)
                || contains_poly_call(else_, name)
        }
        Term::LabelConstruct { payload, .. } => contains_poly_call(payload, name),
        Term::Into { inner }
        | Term::Onto { inner }
        | Term::Iso { inner }
        | Term::Align { inner }
        | Term::Ease { inner }
        | Term::Atom { inner }
        | Term::ReorderSum { inner }
        | Term::ReorderProd { inner }
        | Term::NarrowSum { inner }
        | Term::NarrowProd { inner }
        | Term::WidenSum { inner }
        | Term::WidenProd { inner }
        | Term::FlattenSum { inner }
        | Term::FlattenProd { inner }
        | Term::OneSum { inner }
        | Term::OneProd { inner }
        | Term::Fit { inner } => contains_poly_call(inner, name),
        Term::Ufcs {
            receiver,
            rest_args,
            ..
        } => {
            contains_poly_call(receiver, name)
                || rest_args.iter().any(|a| contains_poly_call(a, name))
        }
        Term::FnPlaceholder { body, .. } => contains_poly_call(body, name),
        Term::Placeholder { .. } => false,
        Term::LiteralAliasRef { .. } => false,
        Term::OpCall { lhs, rhs, .. } => {
            contains_poly_call(lhs, name) || contains_poly_call(rhs, name)
        }
        Term::UserElaborator { inner, .. } => contains_poly_call(inner, name),
        Term::MatchBang {
            scrutinee,
            left_body,
            right_body,
            ..
        } => {
            contains_poly_call(scrutinee, name)
                || contains_poly_call(left_body, name)
                || contains_poly_call(right_body, name)
        }
    }
}
