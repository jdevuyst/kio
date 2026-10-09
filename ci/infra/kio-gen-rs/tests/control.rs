use std::collections::HashMap;

use kio_gen::{
    control::{self, Block, ControlProbe, Descriptor, Payload, Receiver, Sequence, Step, Value},
    emit, generate, mutate, program_seed, shrink,
};

fn sequences(probe: &ControlProbe) -> Vec<&Sequence> {
    match probe {
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
        if let ControlProbe::Blocks {
            first,
            second,
            label,
            nested,
            reverse,
        } = probe
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
