use std::collections::HashMap;

use kio_gen::{
    control::{
        self, Block, ControlProbe, Descriptor, Payload, Receiver, RecursionMode, RecursionProbe,
        RowProbe, Sequence, Step, TypeProbe, Value,
    },
    emit, generate, mutate, program_seed, shrink,
};

fn sequences(probe: &ControlProbe) -> Vec<&Sequence> {
    match probe {
        ControlProbe::Surface { control, .. } => sequences(control),
        ControlProbe::Sequence(s) => vec![s],
        ControlProbe::Blocks { first, second, .. } => [first, second]
            .into_iter()
            .filter_map(|b| {
                if let Block::Sequence(s) = b {
                    Some(s)
                } else {
                    None
                }
            })
            .collect(),
    }
}

fn check_sequence(s: &Sequence, depth: usize) {
    assert!(depth <= 1);
    // `value` always starts in scope with String; Unit binders never replace it.
    assert!(
        s.steps
            .iter()
            .filter(|s| matches!(s, Step::Bind(_) | Step::Action))
            .count()
            <= 3
    );
    assert!(
        s.steps
            .iter()
            .filter(|s| matches!(s, Step::Nested(_)))
            .count()
            <= 1
    );
    for step in &s.steps {
        if let Step::Nested(inner) = step {
            check_sequence(inner, depth + 1);
        }
    }
}

#[test]
fn invoked_control_distribution_covers_semantic_catalog() {
    let mut seen = HashMap::<String, u64>::new();
    for i in 0..2000 {
        let seed = program_seed(0xC017_2015, i);
        let p = generate::program_from_seed(seed);
        let Some(probe) = &p.control_probe else {
            continue;
        };
        let mut witness = |key: String| {
            seen.entry(key).or_insert(seed);
        };
        let files = emit::render_package_files(&p);
        let root = &files
            .iter()
            .find(|(path, _)| path == "workdir/prog.kio")
            .unwrap()
            .1;
        let main = if p.ret == kio_gen::ast::Type::Base(kio_gen::ast::Base::I32) {
            "pub fn main() -> . { control_probe.run(); print_i32(kio_gen_observed()) }"
        } else {
            "pub fn main() -> . { control_probe.run() }"
        };
        assert!(root.contains(main));
        let runtime = &files
            .iter()
            .find(|(path, _)| path == "workdir/kio_gen_control.kio")
            .unwrap()
            .1;
        assert!(runtime.contains("pub fn run()"));
        let descriptors = &files
            .iter()
            .find(|(path, _)| path == "workdir/kio_gen_control_blocks.kio")
            .unwrap()
            .1;
        assert!(descriptors.contains("[Left][Right] ((Left -> Right -> .) & Left & Right) -> ."));
        for s in sequences(probe) {
            check_sequence(s, 0);
            witness(format!("receiver:{:?}", s.receiver));
            let count = s
                .steps
                .iter()
                .filter(|s| matches!(s, Step::Bind(_) | Step::Action))
                .count();
            witness(format!("steps:{count}"));
            for (j, step) in s.steps.iter().enumerate() {
                match step {
                    Step::Let(Value::Current) => witness("reference".into()),
                    Step::Let(Value::Literal(_)) => witness("shadow".into()),
                    Step::Bind(Payload::String(_)) => witness("string-bind".into()),
                    Step::Bind(Payload::Unit) => witness("unit-bind".into()),
                    Step::Action => witness("action".into()),
                    Step::Nested(_) => witness("nested-sequence".into()),
                }
                if matches!(step, Step::Let(_))
                    && s.steps[j + 1..]
                        .iter()
                        .any(|s| matches!(s, Step::Action | Step::Bind(_)))
                {
                    witness("interleaved-let".into());
                }
            }
        }
        let control = if let ControlProbe::Surface { control, .. } = probe {
            control.as_ref()
        } else {
            probe
        };
        if let ControlProbe::Blocks {
            first,
            second,
            label,
            nested,
            reverse,
        } = control
        {
            witness(format!(
                "pair:{:?}:{:?}",
                first.descriptor(),
                second.descriptor()
            ));
            assert!(runtime.contains(&format!("}} {label} {{")));
            assert!(runtime.contains("fn consumer(first:"));
            if *nested {
                assert!(runtime.contains("nested_probe! nested_consumer"));
                witness("nested-descriptor".into());
            }
            witness(format!("reverse:{reverse}"));
            for b in [first, second] {
                if let Block::Product(entries) = b {
                    witness(format!("product:{}", entries.len()));
                }
            }
        }
    }
    for first in [Descriptor::Product, Descriptor::Thunk, Descriptor::Sequence] {
        for second in [Descriptor::Product, Descriptor::Thunk, Descriptor::Sequence] {
            assert!(
                seen.contains_key(&format!("pair:{first:?}:{second:?}")),
                "missing invoked pair {first:?}/{second:?}"
            );
        }
    }
    for key in [
        "receiver:Direct",
        "receiver:Factory",
        "receiver:Projected",
        "steps:0",
        "steps:1",
        "steps:2",
        "steps:3",
        "reference",
        "shadow",
        "string-bind",
        "unit-bind",
        "action",
        "nested-sequence",
        "interleaved-let",
        "nested-descriptor",
        "reverse:true",
        "reverse:false",
        "product:0",
        "product:1",
        "product:2",
    ] {
        assert!(seen.contains_key(key), "missing {key}");
    }
    let mut witnesses: Vec<_> = seen.into_iter().collect();
    witnesses.sort();
    for (key, seed) in witnesses {
        println!("control witness {key}: seed={seed}");
    }
}

#[test]
fn control_source_stdout_and_protocol_are_seed_deterministic() {
    for seed in 0..100 {
        let a = generate::program_from_seed(seed);
        let b = generate::program_from_seed(seed);
        assert_eq!(
            emit::render_package_files(&a),
            emit::render_package_files(&b)
        );
        assert_eq!(
            a.control_probe.as_ref().map(control::expected_stdout),
            b.control_probe.as_ref().map(control::expected_stdout)
        );
        assert_eq!(emit::program_protocol(&a), emit::program_protocol(&b));
    }
}

#[test]
fn invalid_unshrunken_shrunken_and_prime_programs_have_no_runtime_probe() {
    for seed in 0..100 {
        let p = generate::program_from_seed(seed);
        assert!(shrink::minimize_body(&p).control_probe.is_none());
        for mutation in mutate::Mutation::all() {
            for candidate in [p.clone(), shrink::minimize_body(&p)] {
                let (files, _, _) = mutate::apply(&candidate, *mutation);
                assert!(
                    !files
                        .iter()
                        .any(|(path, content)| path.contains("kio_gen_control")
                            || path.contains("kio_gen_types")
                            || path.contains("kio_gen_recursion")
                            || content.contains("control_probe.run()"))
                );
            }
        }
        let prime = generate::program_from_seed_opts(seed, &generate::GenOpts { prime_only: true });
        assert!(prime.control_probe.is_none());
        assert_eq!(
            emit::program_protocol(&prime),
            emit::GENERATED_CORE_MAIN_PROTOCOL
        );
    }
}

#[test]
fn probe_manifest_and_bridge_match_all_backend_print_protocol() {
    let mut p = generate::program_from_seed(0);
    p.surface_mode = true;
    p.control_probe = Some(ControlProbe::Sequence(Sequence {
        receiver: Receiver::Factory,
        initial: "zero\n".into(),
        steps: vec![],
        result: Value::Current,
    }));
    let files = emit::render_package_files(&p);
    let manifest = &files
        .iter()
        .find(|(path, _)| path == "workdir/prog.pkg.kio")
        .unwrap()
        .1;
    assert!(manifest.contains("target (js, \"js\") {"));
    for target in ["ts", "python", "java", "rust", "go", "swift", "haskell"] {
        assert!(manifest.contains(&format!("target {target} {{")));
    }
    assert!(manifest.contains("  testapi/io\n"));
    assert_eq!(
        emit::program_protocol(&p),
        emit::GENERATED_CONTROL_MAIN_PROTOCOL
    );
}

#[test]
fn invoked_type_and_recursion_probes_cover_grammar_cells() {
    let mut seen = HashMap::<String, u64>::new();
    for i in 0..4000 {
        let seed = program_seed(0xC017_2015, i);
        let p = generate::program_from_seed(seed);
        let Some(ControlProbe::Surface {
            types, recursion, ..
        }) = &p.control_probe
        else {
            continue;
        };
        let files = emit::render_package_files(&p);
        let source = &files
            .iter()
            .find(|(path, _)| path == "workdir/kio_gen_types.kio")
            .unwrap()
            .1;
        assert!(source.contains("pub fn transform(input: String) -> String"));
        let key = match types {
            TypeProbe::Staged {
                spelling,
                explicit,
                first,
                last,
            } => {
                assert_ne!(first, last);
                assert!(source.contains("fn staged(prefix: String)[A](value: A) -> String"));
                assert!(source.contains("let selected = "));
                format!("staged:{spelling:?}:{explicit}")
            }
            TypeProbe::HigherRank { .. } => {
                assert!(source.contains("transform: [A] A -> A"));
                assert!(source.contains("apply(identity,"));
                "higher-rank-value".into()
            }
            TypeProbe::HigherKinded { wrapped, .. } => {
                assert!(source.contains("fn apply[*F][A](value: F(A), project: [B] F(B) -> B)"));
                assert!(source.contains("{ apply("));
                format!("higher-kinded:{wrapped}")
            }
            TypeProbe::Existential {
                payload,
                explicit_open,
                first,
                last,
            } => {
                assert!(*payload < 3);
                assert_ne!(first, last);
                assert!(source.contains("newtype Packed <Hidden> : Hidden & (Hidden -> String)"));
                assert!(source.contains("observe(seed)"));
                assert!(source.contains("let selected = open(packed); selected"));
                assert_eq!(source.contains("let .(<Hidden> packet)"), !explicit_open);
                assert_eq!(
                    source.contains("Packed.get(packed)(String, .[Hidden]"),
                    *explicit_open
                );
                format!("existential:{payload}:{explicit_open}")
            }
        };
        seen.entry(key).or_insert(seed);
        let manifest = &files
            .iter()
            .find(|(path, _)| path == "workdir/prog.pkg.kio")
            .unwrap()
            .1;
        assert_eq!(manifest.contains("  testapi/iter\n"), recursion.is_some());
        assert_eq!(
            emit::program_protocol(&p),
            if recursion.is_some() {
                emit::GENERATED_RECURSION_MAIN_PROTOCOL
            } else {
                emit::GENERATED_CONTROL_MAIN_PROTOCOL
            }
        );
        if let Some(probe) = recursion {
            assert!(probe.depth <= 3);
            let source = &files
                .iter()
                .find(|(path, _)| path == "workdir/kio_gen_recursion.kio")
                .unwrap()
                .1;
            assert!(source.contains("{ walk(") || source.contains("; saved()"));
            assert!(source.contains("match! current { (\n.(rest: Counter)"));
            assert!(source.contains("},\n.(_unit: .)"));
            assert_eq!(source.contains("fn bounce"), probe.mutual);
            assert_eq!(
                source.contains("recur(cont)"),
                probe.mode == RecursionMode::Pending
            );
            assert_eq!(
                source.contains("recur(poly)"),
                probe.mode == RecursionMode::Poly
            );
            assert_eq!(
                source.contains("recur(poly, cont)"),
                probe.mode == RecursionMode::PolyPending
            );
            assert_eq!(
                source.contains("recur(escape)"),
                probe.mode == RecursionMode::Escape
            );
            assert_eq!(
                source.contains("recur(poly, escape)"),
                probe.mode == RecursionMode::PolyEscape
            );
            if matches!(
                probe.mode,
                RecursionMode::Escape | RecursionMode::PolyEscape
            ) {
                assert!(source.contains("; io.print(\"saved\\n\"); saved()"));
            }
            seen.entry(format!(
                "recursion:{:?}:{}:{}",
                probe.mode, probe.mutual, probe.depth
            ))
            .or_insert(seed);
        }
    }
    for spelling in kio_gen::ast::UfcsSpelling::ALL {
        for explicit in [false, true] {
            assert!(seen.contains_key(&format!("staged:{spelling:?}:{explicit}")));
        }
    }
    for key in [
        "higher-rank-value",
        "higher-kinded:false",
        "higher-kinded:true",
    ] {
        assert!(seen.contains_key(key), "missing {key}");
    }
    for payload in 0..3 {
        for explicit in [false, true] {
            assert!(seen.contains_key(&format!("existential:{payload}:{explicit}")));
        }
    }
    for mode in [
        RecursionMode::Tail,
        RecursionMode::Pending,
        RecursionMode::Poly,
        RecursionMode::PolyPending,
        RecursionMode::Escape,
        RecursionMode::PolyEscape,
    ] {
        for mutual in [false, true] {
            for depth in 0..=3 {
                assert!(
                    seen.contains_key(&format!("recursion:{mode:?}:{mutual}:{depth}")),
                    "missing {mode:?}/{mutual}/{depth}"
                );
            }
        }
    }
    let mut witnesses: Vec<_> = seen.into_iter().collect();
    witnesses.sort();
    for (key, seed) in witnesses {
        println!("surface grammar witness {key}: seed={seed}");
    }
}

#[test]
fn finite_recursion_oracle_counts_iterations_and_pending_work() {
    let mut probe = RecursionProbe {
        mode: RecursionMode::Tail,
        mutual: false,
        depth: 0,
    };
    assert_eq!(
        control::recursion_stdout(&probe, "chosen\n"),
        "base\nchosen\n"
    );
    probe.depth = 3;
    assert_eq!(
        control::recursion_stdout(&probe, "chosen\n"),
        "step\nstep\nstep\nbase\nchosen\n"
    );
    probe.mode = RecursionMode::Pending;
    assert_eq!(
        control::recursion_stdout(&probe, "chosen\n"),
        "before\nbefore\nbefore\nbase\nafter\nafter\nafter\nchosen\n"
    );
    probe.mode = RecursionMode::Escape;
    assert_eq!(
        control::recursion_stdout(&probe, "chosen\n"),
        "saved\nescape\nescape\nescape\nbase\nchosen\n"
    );
    probe.mode = RecursionMode::PolyEscape;
    assert_eq!(
        control::recursion_stdout(&probe, "chosen\n"),
        "saved\nescape\nescape\nescape\nbase\nchosen\n"
    );
    probe.mode = RecursionMode::PolyPending;
    assert_eq!(
        control::recursion_stdout(&probe, "chosen\n"),
        "before\nbefore\nbefore\nbase\nafter\nafter\nafter\nchosen\n"
    );
    probe.mutual = true;
    assert_eq!(
        control::recursion_stdout(&probe, "chosen\n"),
        "walk:before\nbounce:before\nwalk:before\nbounce:base\nwalk:after\nbounce:after\nwalk:after\nchosen\n"
    );
}

#[test]
fn existential_oracle_preserves_the_selected_sequence_value() {
    for payload in 0..3 {
        for explicit_open in [false, true] {
            let probe = ControlProbe::Surface {
                control: Box::new(ControlProbe::Sequence(Sequence {
                    receiver: Receiver::Direct,
                    initial: "control\n".into(),
                    steps: vec![Step::Let(Value::Literal("selected\n".into()))],
                    result: Value::Current,
                })),
                types: TypeProbe::Existential {
                    payload,
                    explicit_open,
                    first: "first\n".into(),
                    last: "last\n".into(),
                },
                recursion: None,
                rows: None,
            };
            assert_eq!(control::expected_stdout(&probe), "selected\nselected\n");
            let files = kio_gen::control_render::files(&probe);
            let runtime = &files
                .iter()
                .find(|(p, _)| p == "workdir/kio_gen_control.kio")
                .unwrap()
                .1;
            assert!(runtime.contains("let value = control_value(); io.print(value); let selected = type_probe.transform(value); io.print(selected)"));
            assert!(runtime.contains("Box.get(result) }"));
            let types = &files
                .iter()
                .find(|(p, _)| p == "workdir/kio_gen_types.kio")
                .unwrap()
                .1;
            assert!(types.contains("observe(seed)"));
            if payload == 1 {
                assert!(types.contains("((\"last\\n\", input)"));
            }
        }
    }
}

#[test]
fn invoked_row_probes_cover_updates_bindings_and_preserved_fields() {
    let mut seen = HashMap::<String, u64>::new();
    let mut payloads = std::collections::HashSet::new();
    for i in 0..256 {
        let seed = program_seed(0x00F1_E1D5, i);
        let p = generate::program_from_seed(seed);
        let Some(ControlProbe::Surface {
            rows: Some(rows), ..
        }) = &p.control_probe
        else {
            continue;
        };
        let files = emit::render_package_files(&p);
        let source = &files
            .iter()
            .find(|(path, _)| path == "workdir/kio_gen_rows.kio")
            .unwrap()
            .1;
        let driver = &files
            .iter()
            .find(|(path, _)| path == "workdir/kio_gen_control.kio")
            .unwrap()
            .1;
        assert!(driver.contains("import kio_gen_rows as row_probe;"));
        assert!(driver.contains(" row_probe.run();"));
        assert_eq!(
            emit::program_protocol(&p),
            if p.control_probe.as_ref().unwrap().has_recursion() {
                emit::GENERATED_RECURSION_MAIN_PROTOCOL
            } else {
                emit::GENERATED_CONTROL_MAIN_PROTOCOL
            }
        );
        assert!(source.contains("let row = source().!{"));
        assert!(source.contains(" = binding_source(row);"));
        seen.entry(format!("updates:{}", rows.updates.len()))
            .or_insert(seed);
        seen.entry(format!("bindings:{}", rows.bindings.len()))
            .or_insert(seed);
        seen.entry(format!("order:{:?}", rows.order))
            .or_insert(seed);
        for original in &rows.original {
            payloads.insert(original.clone());
        }
        for (index, alias) in &rows.bindings {
            let updated = rows.updates.iter().any(|(field, _)| field == index);
            seen.entry(format!("binding:{alias}:{updated}"))
                .or_insert(seed);
        }
        let expected = control::row_stdout(rows);
        assert!(control::expected_stdout(p.control_probe.as_ref().unwrap()).ends_with(&expected));
        for (index, payload) in &rows.updates {
            assert_ne!(payload, &rows.original[*index]);
            assert!(source.contains(&format!("row-update-{index}\\n")));
            assert!(expected.contains(payload));
        }
    }
    for count in 0..=3 {
        assert!(seen.contains_key(&format!("updates:{count}")), "{seen:?}");
    }
    for count in 1..=3 {
        assert!(seen.contains_key(&format!("bindings:{count}")), "{seen:?}");
    }
    for alias in [false, true] {
        for updated in [false, true] {
            assert!(
                seen.contains_key(&format!("binding:{alias}:{updated}")),
                "{seen:?}"
            );
        }
    }
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        assert!(seen.contains_key(&format!("order:{order:?}")), "{seen:?}");
    }
    assert!(payloads.len() > 32, "row payloads must vary across seeds");
    eprintln!("row probe witnesses: {seen:?}");
}

#[test]
fn row_oracle_distinguishes_lost_updates_and_wrong_bindings() {
    let probe = RowProbe {
        order: [2, 0, 1],
        original: [
            "old-first\n".into(),
            "kept-middle\n".into(),
            "old-last\n".into(),
        ],
        updates: vec![(2, "new-last\n".into()), (0, "new-first\n".into())],
        bindings: vec![(2, true), (1, false), (0, true)],
    };
    let expected = "row-source\nrow-update-2\nrow-update-0\nnew-first\nkept-middle\nnew-last\nrow-bind\nnew-last\nkept-middle\nnew-first\n";
    assert_eq!(control::row_stdout(&probe), expected);
    let mut lost = probe.clone();
    lost.updates.clear();
    assert_ne!(control::row_stdout(&lost), expected);
    let mut swapped = probe.clone();
    swapped.updates[0].1 = "new-first\n".into();
    swapped.updates[1].1 = "new-last\n".into();
    assert_ne!(control::row_stdout(&swapped), expected);
    let mut wrong_binding = probe.clone();
    wrong_binding.bindings[0].0 = 0;
    assert_ne!(control::row_stdout(&wrong_binding), expected);
    let mut missing_binding = probe.clone();
    missing_binding.bindings.remove(0);
    assert_ne!(control::row_stdout(&missing_binding), expected);
}
