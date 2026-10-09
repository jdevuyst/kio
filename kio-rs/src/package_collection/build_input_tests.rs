use super::*;

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

#[test]
fn build_inventory_reuses_one_walk_manifest_and_exact_input_set() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    write(
        &root,
        "app.pkg.kio",
        "package app; build { target js { out \"out/js\"; } }",
    );
    write(&root, "main.kio", "module main; pub fn main() -> . { () }");
    write(
        &root,
        "dep.dep.kio",
        "dependency dep; source { path \"../dep.pkg.kio\"; }",
    );
    write(
        &root,
        "app.lock.kio",
        "lock bytes are read, not parsed by the source walk",
    );
    write(
        &root,
        "app.sig.kio",
        "signature bytes are read, not parsed by the source walk",
    );
    write(&root, "out/generated.kio", "module out/generated;");
    write(&root, ".hidden/private.kio", "module hidden/private;");
    write(&root, "nested/inner.pkg.kio", "package inner;");
    write(
        &root,
        "nested/ignored.kio",
        "deliberately invalid nested-package module",
    );
    BUILD_INPUT_WORK.with_borrow_mut(|slot| *slot = Some(BuildInputWork::default()));
    let mut known = Vec::new();
    let inputs = prepare_build_inputs(&root, &root.join("app.pkg.kio"), &mut known).unwrap();
    BUILD_INPUT_WORK.with_borrow(|slot| {
        let work = slot.as_ref().unwrap();
        assert_eq!((work.inventories, work.manifest_parses), (1, 1));
        assert_eq!(work.reads, [root.join("app.pkg.kio")]);
    });
    let prepared = walk_prepared_build(&inputs).unwrap();
    let work = BUILD_INPUT_WORK.with_borrow_mut(Option::take).unwrap();
    assert_eq!((work.inventories, work.manifest_parses), (1, 1));
    let mut reads = work.reads;
    reads.sort();
    assert_eq!(reads, inputs.files, "each input is read exactly once");
    let ordinary = walk(&root).unwrap();
    assert_eq!(collect_sources(&prepared), collect_sources(&ordinary));
    assert_eq!(inputs.files.len(), 7);
    assert!(known.is_empty(), "ready plan owns its inventory");
}

#[test]
fn build_inventory_storage_does_not_scale_with_module_body_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    write(
        &root,
        "app.pkg.kio",
        "package app; build { target js { out \"out/js\"; } }",
    );
    let mut snapshots = Vec::new();
    for size in [32, 2_000_000] {
        // Invalid source is intentional: preflight has no reason to lex or force it.
        write(&root, "main.kio", &"?".repeat(size));
        BUILD_INPUT_WORK.with_borrow_mut(|slot| *slot = Some(BuildInputWork::default()));
        let inputs =
            prepare_build_inputs(&root, &root.join("app.pkg.kio"), &mut Vec::new()).unwrap();
        let work = BUILD_INPUT_WORK.with_borrow_mut(Option::take).unwrap();
        assert_eq!(work.reads, [root.join("app.pkg.kio")]);
        snapshots.push((
            inputs.files,
            inputs.manifest_source,
            format!("{:?}", inputs.manifest),
        ));
    }
    assert_eq!(snapshots[0], snapshots[1]);
}

#[cfg(unix)]
#[test]
fn build_inventory_keeps_file_symlinks_but_does_not_follow_directory_symlinks() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("app");
    write(&root, "app.pkg.kio", "package app;");
    write(dir.path(), "outside/main.kio", "module main;");
    symlink(dir.path().join("outside/main.kio"), root.join("main.kio")).unwrap();
    symlink(dir.path().join("outside"), root.join("linked")).unwrap();
    let inputs = prepare_build_inputs(&root, &root.join("app.pkg.kio"), &mut Vec::new()).unwrap();
    assert_eq!(inputs.files.len(), 2);
    assert!(inputs.files.contains(&root.join("main.kio")));
    assert!(
        !inputs
            .files
            .iter()
            .any(|path| path.starts_with(root.join("linked")))
    );
}

#[cfg(feature = "parallel")]
#[test]
fn file_parses_overlap_and_merge_in_input_order() {
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let files = vec![root.join("a.kio"), root.join("b.kio")];
    let overlay = SourceOverlay::complete(
        root.to_path_buf(),
        BTreeMap::from([
            (files[0].clone(), "module a; fn f() -> . { () }".into()),
            (files[1].clone(), "module b; fn f() -> . { () }".into()),
        ]),
    );
    let completed = Mutex::new(Vec::new());
    let ready = Condvar::new();
    let parsed = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap()
        .install(|| {
            parse_package_file_list_with_parser(root, &overlay, &files, None, |path, source| {
                if path == files[0] {
                    let (done, timeout) = ready
                        .wait_timeout_while(
                            completed.lock().unwrap(),
                            Duration::from_secs(2),
                            |done| done.is_empty(),
                        )
                        .unwrap();
                    assert!(
                        !timeout.timed_out(),
                        "another file must parse while the first waits"
                    );
                    assert_eq!(*done, [files[1].clone()]);
                }
                let parsed = crate::pass::parser::parse_module_file_lazy(source);
                completed.lock().unwrap().push(path.to_path_buf());
                ready.notify_all();
                parsed
            })
        })
        .unwrap();
    assert_eq!(
        *completed.lock().unwrap(),
        [files[1].clone(), files[0].clone()]
    );
    assert_eq!(
        parsed
            .modules
            .iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>(),
        files.iter().collect::<Vec<_>>()
    );
    assert_eq!(parsed.sources, overlay.files);
}

#[test]
fn file_parse_errors_keep_input_order_across_read_and_manifest_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broken = root.join("a.kio");
    let missing = root.join("b.kio");
    let manifest = root.join("c.pkg.kio");
    let source = "module a; fn";
    let overlay = SourceOverlay::complete(
        root.to_path_buf(),
        BTreeMap::from([
            (broken.clone(), source.into()),
            (manifest.clone(), "package".into()),
        ]),
    );
    let error = parse_package_file_list(root, &overlay, &[broken.clone(), missing.clone()], None)
        .unwrap_err();
    let WalkError::Parse {
        path,
        source_text,
        error,
    } = error
    else {
        panic!("earlier parse failure wins")
    };
    assert_eq!(path, broken);
    assert_eq!(source_text, source);
    assert_eq!(
        error,
        crate::pass::parser::parse_module_file_lazy(source).unwrap_err()
    );
    assert!(
        matches!(parse_package_file_list(root, &overlay, &[missing.clone(), broken.clone()], None).unwrap_err(), WalkError::ReadFile { path, .. } if path == missing)
    );
    assert!(
        matches!(parse_package_file_list(root, &overlay, &[manifest.clone(), broken], None).unwrap_err(), WalkError::Parse { path, .. } if path == manifest)
    );
}

#[test]
fn file_parse_preserves_local_grammar_ids_and_deferred_bodies() {
    use crate::pass::parser::{self, IMPORT_GRAMMAR_CONSUMER};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let sources = BTreeMap::from([
        (
            root.join("app/main.kio"),
            IMPORT_GRAMMAR_CONSUMER.to_owned(),
        ),
        (
            root.join("broken.kio"),
            "module broken; fn bad() -> . { let x = ; () }".to_owned(),
        ),
        (
            root.join("plain.kio"),
            "module plain; fn first(a: .) -> . { a.>forward } fn second(a: .) -> . { pass!(a.>forward) }".to_owned(),
        ),
    ]);
    let overlay = SourceOverlay::complete(root.to_path_buf(), sources.clone());
    let parsed = walk_with_overlay(root, &overlay).unwrap();
    let package = &parsed.packages[&parsed.root];
    assert_eq!(package.sources, sources);
    assert_eq!(package.modules.len(), 3);
    for (path, module) in &package.modules {
        let sequential = parser::parse_module_file_lazy(&sources[path]).unwrap();
        assert_eq!(*module, sequential.module);
        let actual = package.lazy_modules[path].force_all();
        let expected = sequential.force_all().map(|file| file.module);
        assert_eq!(actual, expected, "{}", path.display());
    }
    parser::import_grammar_assert_consumer(
        &package.lazy_modules[&root.join("app/main.kio")]
            .force_all()
            .unwrap(),
    );
    assert!(
        package.lazy_modules[&root.join("broken.kio")]
            .force_all()
            .is_err()
    );
}
