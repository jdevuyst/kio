use super::{
    parse, parse_build_block_body, parse_dependency_file, parse_lock_file, parse_package_file,
    parse_signature_file,
};
use crate::pretty::{pretty_dependency_file, pretty_lock_file, pretty_module, pretty_package_file};

#[derive(Clone, Copy, Debug)]
enum Kind {
    Module,
    Package,
    Dependency,
    Lock,
    Signature,
}

fn format_source(kind: Kind, source: &str) -> Result<String, String> {
    match kind {
        Kind::Module => parse(source).map(|value| pretty_module(&value)),
        Kind::Package => parse_package_file(source, None).map(|value| pretty_package_file(&value)),
        Kind::Dependency => {
            parse_dependency_file(source, None).map(|value| pretty_dependency_file(&value))
        }
        Kind::Lock => parse_lock_file(source, None).map(|value| pretty_lock_file(&value)),
        Kind::Signature => {
            parse_signature_file(source, None).map(|value| crate::sig::emit_signature_file(&value))
        }
    }
    .map_err(|error| format!("{error:?}"))
}

fn orders<'a>(entries: &[&'a str]) -> Vec<Vec<&'a str>> {
    if entries.is_empty() {
        return vec![Vec::new()];
    }
    let mut result = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let mut rest = entries.to_vec();
        rest.remove(index);
        for mut order in orders(&rest) {
            order.insert(0, entry);
            result.push(order);
        }
    }
    result
}

struct Permutations {
    name: &'static str,
    kind: Kind,
    prefix: String,
    entries: Vec<&'static str>,
    suffix: &'static str,
}

#[test]
fn named_entry_permutations_preserve_canonical_output() {
    let mut groups = vec![
        Permutations {
            name: "build fields",
            kind: Kind::Package,
            prefix: "package app; build { ".into(),
            entries: vec![
                "cache \"cache-dir\";",
                "docs { md \"docs\"; support \"first\"; support \"second\"; };",
                "target rust { out \"rust-out\"; };",
            ],
            suffix: " }",
        },
        Permutations {
            name: "package sections",
            kind: Kind::Package,
            prefix: "package app; ".into(),
            entries: vec!["build { cache (); }", "bridge { app; }"],
            suffix: "",
        },
        Permutations {
            name: "dependency sections",
            kind: Kind::Dependency,
            prefix: "dependency lib; ".into(),
            entries: vec![
                "source { path \"lib.pkg.kio\"; }",
                "rehost lib/io to app/io;",
                "retype lib/types to app/types;",
            ],
            suffix: "",
        },
        Permutations {
            name: "elaborator fields",
            kind: Kind::Module,
            prefix: "module app; elab make: . { ".into(),
            entries: vec!["captures (first, second);", "impl(fills) run;"],
            suffix: " };",
        },
        Permutations {
            name: "signature version sections",
            kind: Kind::Signature,
            prefix: "signature app v(1); v(1) { ".into(),
            entries: vec![
                "with { module api { pub rec newtype A : . | A { pub constructor make; pub projector read; }; } };",
                "breaking { add { module api { host type H; } } };",
                "nonbreaking { add { api.A; } };",
            ],
            suffix: " }",
        },
        Permutations {
            name: "signature change buckets",
            kind: Kind::Signature,
            prefix: "signature app v(2); v(1) { nonbreaking { add { module api { pub type Old = .; pub type Gone = .; } } } } v(2) { nonbreaking { ".into(),
            entries: vec![
                "add { module api { pub type Fresh = .; } };",
                "modify { module api { pub type Old = !; } };",
                "remove { module api { Gone; } };",
            ],
            suffix: " } }",
        },
        // Existing unordered bodies are controls, not new language features.
        Permutations {
            name: "source fields",
            kind: Kind::Dependency,
            prefix: "dependency lib; source { ".into(),
            entries: vec!["git \"https://example.invalid/lib\";", "ref \"main\";"],
            suffix: " }",
        },
        Permutations {
            name: "newtype members",
            kind: Kind::Module,
            prefix: "module app; newtype Box : . { ".into(),
            entries: vec!["pub constructor make;", "projector read;"],
            suffix: " };",
        },
    ];
    for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
        let primary = match mode {
            "foldl" => "foldl step initial;",
            "foldr" => "foldr step initial;",
            "foldl1" => "foldl1 step initial;",
            "foldr1" => "foldr1 step initial;",
            _ => unreachable!("the table contains the four declared modes"),
        };
        groups.push(Permutations {
            name: mode,
            kind: Kind::Module,
            prefix: "module app; varop [* *] { ".into(),
            entries: vec![primary, "finalize finish;"],
            suffix: " };",
        });
    }

    let mut failures = Vec::new();
    let mut visited = 0;
    for group in groups {
        let canonical_source = format!(
            "{}{}{}",
            group.prefix,
            group.entries.join(" "),
            group.suffix
        );
        let expected = format_source(group.kind, &canonical_source);
        for order in orders(&group.entries) {
            visited += 1;
            let source = format!("{}{}{}", group.prefix, order.join(" "), group.suffix);
            let check: Result<(), String> = (|| {
                let expected = expected.as_ref().map_err(Clone::clone)?;
                let actual = format_source(group.kind, &source)?;
                if &actual != expected {
                    return Err(format!(
                        "canonical output differs:\n{actual}\nexpected:\n{expected}"
                    ));
                }
                if format_source(group.kind, &actual)? != actual {
                    return Err("formatting is not idempotent".into());
                }
                Ok(())
            })();
            if let Err(error) = check {
                failures.push(format!("{}: {source}\n{error}", group.name));
            }
        }
    }
    assert_eq!(
        visited, 40,
        "every independently constructed permutation runs"
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn reordered_build_fields_preserve_repeated_entries_comments_and_spans() {
    let entries = [
        "// cache note\ncache \"cache-dir\";",
        "// docs note\ndocs { md \"docs\"; support \"first\"; support \"second\"; };",
        "// first target\ntarget rust { out \"rust-out\"; };",
        "// second target\ntarget js { out \"js-out\"; };",
    ];
    let mut failures = Vec::new();
    let mut visited = 0;
    let expected = format_source(
        Kind::Package,
        &format!("package app; build {{\n{}\n}}", entries.join("\n")),
    );
    for order in orders(&entries) {
        if order.iter().position(|entry| entry.contains("target rust"))
            > order.iter().position(|entry| entry.contains("target js"))
        {
            continue;
        }
        visited += 1;
        let body = order.join("\n");
        let source = format!("package app; build {{\n{body}\n}}");
        let check: Result<(), String> = (|| {
            let build = parse_build_block_body(&body).map_err(|error| format!("{error:?}"))?;
            let child_end = body
                .strip_suffix(';')
                .expect("each entry has one separator")
                .len();
            if build.span.end as usize != child_end {
                return Err(format!(
                    "body span ends at {}, not {}",
                    build.span.end, child_end
                ));
            }
            let without_trailing =
                parse_build_block_body(&body[..child_end]).map_err(|error| format!("{error:?}"))?;
            if build != without_trailing {
                return Err("optional trailing separator changed build fields or spans".into());
            }
            let package =
                parse_package_file(&source, None).map_err(|error| format!("{error:?}"))?;
            if package.meta.span.end as usize != source.len() {
                return Err("package span does not reach the last written section".into());
            }
            let actual = pretty_package_file(&package);
            if &actual != expected.as_ref().map_err(Clone::clone)? {
                return Err(format!(
                    "comments or named entries changed canonical output:\n{actual}"
                ));
            }
            for comment in [
                "// cache note",
                "// docs note",
                "// first target",
                "// second target",
            ] {
                if actual.matches(comment).count() != 1 {
                    return Err(format!("comment {comment:?} was lost or duplicated"));
                }
            }
            if build
                .targets
                .iter()
                .map(|target| target.id.as_str())
                .collect::<Vec<_>>()
                != ["rust", "js"]
            {
                return Err("target relative order changed".into());
            }
            if build.docs.as_ref().map(|docs| docs.support.as_slice())
                != Some(&["first".to_owned(), "second".to_owned()][..])
            {
                return Err("support relative order changed".into());
            }
            Ok(())
        })();
        if let Err(error) = check {
            failures.push(format!("{source}\n{error}"));
        }
    }
    assert_eq!(visited, 12);
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn reordered_signature_sections_preserve_semantic_replay() {
    let header = "signature app v(2); v(1) { nonbreaking { add { module api { pub type Old = .; pub type Gone = .; } } } } v(2) {";
    let buckets = [
        "add { api.Fresh; };",
        "modify { module api { pub type Old = !; } };",
        "remove { module api { Gone; } };",
    ];
    let with = "with { module api { pub rec newtype Fresh : . | Fresh { pub constructor make; pub projector read; }; } };";
    let breaking = "breaking { add { module api { host type H; } } };";
    let canonical = format!(
        "{header} {with} {breaking} nonbreaking {{ {} }} }}",
        buckets.join(" ")
    );
    let baseline = parse_signature_file(&canonical, None).expect("canonical history parses");
    let baseline = crate::sig::replay(&baseline).expect("canonical history is semantically valid");
    assert_eq!(baseline.current.items.len(), 3);
    assert_eq!(baseline.removed.len(), 1);
    let mut visited = 0;
    let mut failures = Vec::new();
    for bucket_order in orders(&buckets) {
        let nonbreaking = format!("nonbreaking {{ {} }};", bucket_order.join(" "));
        for sections in orders(&[with, breaking, &nonbreaking]) {
            visited += 1;
            let source = format!("{header} {} }}", sections.join(" "));
            let check: Result<(), String> = (|| {
                let parsed =
                    parse_signature_file(&source, None).map_err(|error| format!("{error:?}"))?;
                let replayed = crate::sig::replay(&parsed).map_err(|error| format!("{error:?}"))?;
                if replayed.current != baseline.current {
                    return Err("live contract changed with field order".into());
                }
                let removed = |value: crate::sig::ReplayedInterface| {
                    value
                        .removed
                        .into_iter()
                        .map(|item| (item.entry, item.removed_at_version))
                        .collect::<Vec<_>>()
                };
                if removed(replayed) != removed(baseline.clone()) {
                    return Err("retirement contract or generation changed with field order".into());
                }
                Ok(())
            })();
            if let Err(error) = check {
                failures.push(format!("{source}\n{error}"));
            }
        }
    }
    assert_eq!(visited, 36);
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn unordered_entries_still_require_unique_compatible_fields() {
    let cases = [
        (Kind::Module, "module app; op ~ _ {} ;"),
        (
            Kind::Module,
            "module app; op ~ _ { impl run; impl other; };",
        ),
        (
            Kind::Module,
            "module app; varop [* *] { finalize finish; };",
        ),
        (
            Kind::Module,
            "module app; varop [* *] { foldl step base; foldr step base; };",
        ),
        (
            Kind::Module,
            "module app; varop [* *] { foldl step base; finalize finish; finalize other; };",
        ),
        (
            Kind::Module,
            "module app; elab make: . { captures (first); };",
        ),
        (
            Kind::Module,
            "module app; elab make: . { captures (first); captures (second); impl run; };",
        ),
        (
            Kind::Module,
            "module app; elab make: . { impl run; impl(fills) other; };",
        ),
        (
            Kind::Module,
            "module app; newtype Box : . { projector read; };",
        ),
        (
            Kind::Module,
            "module app; newtype Box : . { projector read; constructor make; projector other; };",
        ),
        (
            Kind::Package,
            "package app; build { cache (); cache \"other\"; }",
        ),
        (
            Kind::Package,
            "package app; build { docs { md \"one\"; } docs { md \"two\"; } }",
        ),
        (Kind::Package, "package app; build {} build {}"),
        (Kind::Package, "package app; bridge {} bridge {}"),
        (
            Kind::Package,
            "package app; build { docs { html \"out\"; } }",
        ),
        (Kind::Dependency, "dependency lib; rehost lib/io to app/io;"),
        (
            Kind::Dependency,
            "dependency lib; source { path \"one.pkg.kio\"; } source { path \"two.pkg.kio\"; }",
        ),
        (
            Kind::Dependency,
            "dependency lib; source { ref \"main\"; path \"lib.pkg.kio\"; }",
        ),
        (
            Kind::Dependency,
            "dependency lib; source { ref \"main\"; git \"url\"; ref \"other\"; }",
        ),
        (
            Kind::Lock,
            "lock lib; resolved { sig \"digest\"; commit \"hash\"; ref \"main\"; }",
        ),
        (
            Kind::Signature,
            "signature app v(1); v(1) { with { module api { pub type A = .; } } }",
        ),
        (
            Kind::Signature,
            "signature app v(1); v(1) { nonbreaking { add { module api { pub type A = .; } } } nonbreaking { add { module api { pub type B = .; } } } }",
        ),
        (
            Kind::Signature,
            "signature app v(1); v(1) { nonbreaking { add { module api { pub type A = .; } } add { module api { pub type B = .; } } } }",
        ),
    ];
    let mut failures = Vec::new();
    for (kind, source) in cases {
        if let Ok(formatted) = format_source(kind, source) {
            failures.push(format!(
                "{kind:?} accepted invalid fields:\n{source}\n{formatted}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
