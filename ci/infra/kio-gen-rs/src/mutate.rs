//! Mutators that turn a valid Kio' program into an invalid one with
//! a labeled error category.
//!
//! Each mutator commits in advance to which exit code from
//! `specs/exit-codes.md` it intends to fire. The runner enforces that
//! claim by diffing the actual exit code against `expected.exit`,
//! so a "should be parse error" case that silently parses is caught
//! as a generator bug, not an implementation failure.
//!
//! Stage-1 coverable mutations: 11 (parse), 12 (import), 13 (name
//! resolution), 14 (type). Code 15 (elaborator) is gated on Kio surface
//! work and arrives in stage 2.

use rand::Rng;
use rand::seq::SliceRandom;

use kio_gen::ast::{Base, Program, Term, Type};
use kio_gen::emit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// 11 — append a stray token to prog.kio so the parser bails.
    Parse,
    /// 12 — import an unexported item from a helper module so
    /// import resolution fails.
    Import,
    /// 13 — wrap the body in `let __bug = unbound_xyz; ...` so
    /// name resolution fails.
    NameRes,
    /// 14 — wrap the body in `let __bug = "wrong"; ...` so the
    /// typechecker rejects the binding.
    Type,
    /// 15 — append a top-level `fn kio_gen_elaborator_bug() -> I32 {
    /// into!("…") }` so the elaborator fires at the fn-return check
    /// position with no String→I32 coercion available.
    Elaborator,
}

impl Mutation {
    pub fn all() -> &'static [Mutation] {
        &[
            Mutation::Parse,
            Mutation::Import,
            Mutation::NameRes,
            Mutation::Type,
            Mutation::Elaborator,
        ]
    }

    pub fn category(self) -> &'static str {
        match self {
            Mutation::Parse => "11_parse_error",
            Mutation::Import => "12_import_error",
            Mutation::NameRes => "13_name_resolution_error",
            Mutation::Type => "14_type_error",
            Mutation::Elaborator => "15_elaborator_error",
        }
    }

    pub fn exit_code(self) -> u32 {
        match self {
            Mutation::Parse => 11,
            Mutation::Import => 12,
            Mutation::NameRes => 13,
            Mutation::Type => 14,
            Mutation::Elaborator => 15,
        }
    }

    /// Whether the mutated source still parses against the formal
    /// Kio' grammar. Parse-error mutations break it by construction
    /// (a stray `@` token is the whole point), and elaborator mutations
    /// inject a surface `into!(…)` form that's not in Kio' either.
    /// Import / name-res / type mutations leave the source grammatical
    /// and only break later compiler passes — those cases keep the
    /// marker.
    pub fn preserves_kio_prime_grammar(self) -> bool {
        match self {
            Mutation::Parse | Mutation::Elaborator => false,
            Mutation::Import | Mutation::NameRes | Mutation::Type => true,
        }
    }
}

pub fn pick<R: Rng>(rng: &mut R) -> Mutation {
    *Mutation::all().choose(rng).unwrap()
}

/// Apply a mutation to a valid program. Returns the mutated package
/// files alongside the labeled exit code and category.
pub fn apply(prog: &Program, mutation: Mutation) -> (Vec<(String, String)>, u32, &'static str) {
    let prog_for_render = match mutation {
        Mutation::NameRes => wrap_with_unbound_let(prog),
        Mutation::Type => wrap_with_type_mismatch_let(prog),
        Mutation::Parse | Mutation::Import | Mutation::Elaborator => prog.clone(),
    };
    let mut files = emit::render_package_files(&prog_for_render);

    match mutation {
        Mutation::Parse => corrupt_parse(&mut files),
        Mutation::Import => corrupt_import(&mut files),
        Mutation::Elaborator => corrupt_elaborator(&mut files),
        Mutation::NameRes | Mutation::Type => {}
    }

    (files, mutation.exit_code(), mutation.category())
}

fn wrap_with_unbound_let(prog: &Program) -> Program {
    let mut p = prog.clone();
    p.body = Term::Let {
        name: "kio_gen_bug".to_string(),
        ty: Type::Base(Base::I32),
        rhs: Box::new(Term::Var("kio_gen_unbound".to_string())),
        body: Box::new(p.body),
    };
    p
}

fn wrap_with_type_mismatch_let(prog: &Program) -> Program {
    let mut p = prog.clone();
    // RHS is `__left__(I32, Bool, "wrong")` — `[A]` binds to `I32`,
    // the value-arg is `String`, mismatch at the intrinsic's value
    // slot. Forward-flow only, independent of checked-let semantics:
    // this mutation should fail because the call is malformed, not
    // because a surrounding binder annotation checks the RHS.
    p.body = Term::Let {
        name: "kio_gen_bug".to_string(),
        ty: Type::Base(Base::I32),
        rhs: Box::new(Term::PolyCall {
            name: "__left__".to_string(),
            type_args: vec![Type::Base(Base::I32), Type::Base(Base::Bool)],
            args: vec![Term::StrLit("wrong".to_string())],
        }),
        body: Box::new(p.body),
    };
    p
}

fn prog_kio_mut(files: &mut [(String, String)]) -> &mut String {
    files
        .iter_mut()
        .find(|(name, _)| name == "workdir/prog.kio")
        .map(|(_, c)| c)
        .expect("emit::render_package_files always produces a prog.kio entry")
}

fn package_kio_mut(files: &mut [(String, String)]) -> &mut String {
    files
        .iter_mut()
        .find(|(name, _)| name == "workdir/prog.pkg.kio")
        .map(|(_, c)| c)
        .expect("emit::render_package_files always produces a prog.pkg.kio entry")
}

/// Add a module root to the package's `bridge { … }` block so a
/// mutation-introduced module participates in the package boundary.
/// Existing roots retain their order; an already-listed root is a no-op.
fn admit_bridge(files: &mut [(String, String)], name: &str) {
    let package = package_kio_mut(files);
    if emit::package_lists_bridge_glob(package, name) {
        return;
    }
    // Mutation inputs come from render_package_files, whose final block
    // carries only ordered module roots rendered by bridge_glob_block.
    let (prefix, bridge) = package
        .split_once("bridge {\n")
        .expect("render_package_files produces a bridge block");
    let body = bridge
        .strip_suffix("}\n")
        .expect("render_package_files ends with its bridge block");
    let roots = body
        .lines()
        .map(|line| line.trim().trim_end_matches(';'))
        .chain(std::iter::once(name));
    *package = format!("{prefix}{}", emit::bridge_glob_block(roots));
}

fn admit_root_module(files: &mut [(String, String)], name: &str) {
    admit_bridge(files, name);
}

fn corrupt_parse(files: &mut [(String, String)]) {
    // `@` is not a valid Kio token outside string literals; appending
    // it after the fn body forces the lexer/parser to reject.
    let source = prog_kio_mut(files);
    source.push_str("\n@\n");
}

fn corrupt_elaborator(files: &mut Vec<(String, String)>) {
    // Append a top-level `fn` whose body is `into!("…")` checked
    // against an `I32` return type. The fn-return position is a
    // check site for `into!`, and there is no `String → I32`
    // coercion the elaborator can fall through, so kio-rs reports an
    // `into!`-no-coercion error and exits 15.
    //
    // We pick an `I32` literal whose role type is certain to be in
    // scope — every kio-gen package's host declares all
    // 14 base role types, including I32. The String literal
    // `"kio_gen_elaborator_bug"` doubles as a load-bearing
    // source-search needle.
    //
    // `into!` must be imported from its same-package implementation.
    // The base program is emitted in surface mode only about half the
    // time, and this mutation appends the `into!` call to the raw text,
    // so inject the import and support files when absent. Otherwise the
    // call would fail before the intended exit 15 no-coercion error.
    emit::add_elaborator_support_files(files);
    // Of the bundled support modules only `testapi` declares `host`
    // items; it is reachable from the bridged `prog` module through the
    // injected `algebraic_elaborators` import, so the bridge-
    // completeness check requires it in the package's bridge globs. The
    // other elaborator modules carry no host items and need no glob.
    admit_root_module(files, "testapi");
    let source = prog_kio_mut(files);
    if !source.contains("import algebraic_elaborators(") {
        // Insert the import right after the `import __intrinsics__;` line.
        let anchor = "import __intrinsics__;\n";
        if let Some(pos) = source.find(anchor) {
            let at = pos + anchor.len();
            source.insert_str(at, "import algebraic_elaborators(into);\n");
        } else {
            // No intrinsics line (shouldn't happen for the generated
            // header, but stay robust): prepend after the module line.
            let mlead = "module prog;\n";
            if let Some(pos) = source.find(mlead) {
                let at = pos + mlead.len();
                source.insert_str(at, "\nimport algebraic_elaborators(into);\n");
            }
        }
    }
    source.push_str(
        "\npub fn kio_gen_elaborator_bug() -> I32 {\n  into!(\"kio_gen_elaborator_bug\")\n}\n",
    );
}

fn corrupt_import(files: &mut Vec<(String, String)>) {
    // Inject an import that asks an existing helper module for
    // an item it doesn't export, before the generated root module's
    // first item. Per
    // `specs/exit-codes.md`, "the module does not export the
    // requested `pub` identifier" is exit 12 (import error) — an import
    // error rather than a name-resolution error.
    //
    // The helper module is deliberately empty. Importing from a
    // present module keeps the error in the missing-export bucket
    // instead of turning it into an unknown-module error. The import
    // is injected into the module's leading import-clause run (an import
    // clause must precede every item, including the `host type`
    // declarations) so the source stays grammatical and the error is
    // the intended missing-export import error, not a parse error.
    if !files
        .iter()
        .any(|(name, _)| name == "workdir/kio_gen_empty.kio")
    {
        files.push((
            "workdir/kio_gen_empty.kio".to_string(),
            "module kio_gen_empty;\n".to_string(),
        ));
    }
    admit_bridge(files, "kio_gen_empty");
    let source = prog_kio_mut(files);
    let inject = "import kio_gen_empty(kio_gen_unexported_item);\n";
    // Anchor on `import __intrinsics__;`, always the module's first import
    // clause; insert immediately after it, keeping the new import inside
    // the leading import-clause run.
    let anchor = "import __intrinsics__;\n";
    if let Some(pos) = source.find(anchor) {
        let at = pos + anchor.len();
        source.insert_str(at, inject);
    } else {
        // Fallback: prepend at the top so the mutated source still
        // exits non-zero (import-error or parse-error, both invalid).
        source.insert_str(0, inject);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kio_gen::shrink;

    fn program(surface_mode: bool) -> Program {
        Program {
            fn_def_name: "gen".to_owned(),
            params: vec![("value".to_owned(), Type::Unit)],
            ret: Type::Unit,
            body: Term::Var("value".to_owned()),
            user_elaborators: Vec::new(),
            uses_surface: false,
            surface_mode,
        }
    }

    fn root(files: &[(String, String)]) -> &str {
        &files
            .iter()
            .find(|(path, _)| path == "workdir/prog.kio")
            .expect("generated root module")
            .1
    }

    #[test]
    fn mutations_keep_rendered_bridge_peer_separators() {
        for surface_mode in [false, true] {
            let prog = program(surface_mode);
            for candidate in [prog.clone(), shrink::minimize_body(&prog)] {
                let mut original = emit::render_package_files(&candidate);
                let original_package = package_kio_mut(&mut original);
                let (prefix, _) = original_package.split_once("bridge {\n").unwrap();
                for mutation in [Mutation::Import, Mutation::Elaborator] {
                    let (mut files, exit, category) = apply(&candidate, mutation);
                    assert_eq!(
                        (exit, category),
                        (mutation.exit_code(), mutation.category())
                    );
                    let (name, bridge) = match (mutation, surface_mode) {
                        (Mutation::Import, false) => {
                            ("kio_gen_empty", "bridge {\n  prog;\n  kio_gen_empty\n}\n")
                        }
                        (Mutation::Import, true) => (
                            "kio_gen_empty",
                            "bridge {\n  prog;\n  testapi;\n  kio_gen_empty\n}\n",
                        ),
                        (Mutation::Elaborator, _) => {
                            ("testapi", "bridge {\n  prog;\n  testapi\n}\n")
                        }
                        _ => unreachable!(),
                    };
                    assert_eq!(package_kio_mut(&mut files), &format!("{prefix}{bridge}"));
                    let before = files.clone();
                    admit_bridge(&mut files, name);
                    assert_eq!(files, before, "admitting an existing root must be a no-op");
                }
            }
        }
    }

    #[test]
    fn import_mutation_preserves_category_and_header_after_shrinking() {
        for surface_mode in [false, true] {
            let prog = program(surface_mode);
            for candidate in [prog.clone(), shrink::minimize_body(&prog)] {
                let (files, exit, category) = apply(&candidate, Mutation::Import);
                assert_eq!((exit, category), (12, "12_import_error"));
                assert!(Mutation::Import.preserves_kio_prime_grammar());
                let source = root(&files);
                let import = "import kio_gen_empty(kio_gen_unexported_item);";
                assert_eq!(source.matches(import).count(), 1);
                let position = source.find(import).unwrap();
                assert!(source.find("import __intrinsics__;").unwrap() < position);
                assert!(position < source.find("host type I8").unwrap());
                assert!(files.iter().any(|(path, source)| {
                    path == "workdir/kio_gen_empty.kio" && source == "module kio_gen_empty;\n"
                }));
            }
        }
    }

    #[test]
    fn elaborator_mutation_injects_only_a_missing_canonical_import() {
        for surface_mode in [false, true] {
            let prog = shrink::minimize_body(&program(surface_mode));
            let (files, exit, category) = apply(&prog, Mutation::Elaborator);
            assert_eq!((exit, category), (15, "15_elaborator_error"));
            assert!(!Mutation::Elaborator.preserves_kio_prime_grammar());
            let source = root(&files);
            assert_eq!(source.matches("import algebraic_elaborators(").count(), 1);
            if !surface_mode {
                assert!(
                    source.contains("import __intrinsics__;\nimport algebraic_elaborators(into);")
                );
            }
            assert!(source.contains("into!(\"kio_gen_elaborator_bug\")"));
        }
    }
}
