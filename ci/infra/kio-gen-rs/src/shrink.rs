//! Reducing invalid programs to a minimal repro.
//!
//! Mutators always wrap a known-bad construct around (or beside) the
//! generator's body — `let _ = "wrong"; <body>`, an extra
//! `@` after the fn, a redirected import, and so on. The body
//! itself is incidental to the failure. Shrinking takes advantage:
//! before mutating, replace the body with a minimal inhabitant of
//! the program's return type and drop the fn's parameters, then
//! let the mutator wrap that. The result still fires the same error
//! category but is far easier to read and to promote into a
//! hand-written golden.
//!
//! Valid programs are never shrunk — the body's structure is the
//! whole point.

use kio_gen::ast::{Base, Program, Term, Type};
use kio_gen::render::term_uses_surface_forms;

/// Reduce an invalid-bound program to its minimal invalid skeleton.
/// The caller is expected to mutate the result; the mutated case
/// still triggers the labeled exit code because the failure mode
/// lives in the mutator's wrapping, not in the body.
pub fn minimize_body(prog: &Program) -> Program {
    let body = minimum_for(&prog.ret);
    Program {
        fn_def_name: prog.fn_def_name.clone(),
        params: vec![],
        ret: prog.ret.clone(),
        user_elaborators: prog.user_elaborators.clone(),
        // Recompute the surface flag from the new body — shrunken
        // bodies often drop the construct that triggered the
        // generator's surface-mode decision. `surface_mode` (the
        // generation-time bit that controls generated helper content) stays
        // pinned to the parent's value: the shrunk program inherits
        // the parent's label-wrapper exposure regardless of whether
        // the new body actually uses labels.
        uses_surface: prog.uses_surface && term_uses_surface_forms(&body),
        surface_mode: prog.surface_mode,
        body,
    }
}

/// A minimal inhabitant of `t`: a literal for bases, `()` for Unit,
/// a constant lambda for functions, `__pair__` of minimums for
/// products, `__left__` of the left minimum for sums, and the
/// declared constructor for nominal newtypes.
fn minimum_for(t: &Type) -> Term {
    match t {
        Type::Base(b) => match b {
            Base::Str => Term::StrLit(String::new()),
            Base::Bool => Term::BoolLit(false),
            Base::F32 | Base::F64 => Term::NumLit {
                base: *b,
                repr: "0.0".to_string(),
            },
            // every integer base: use "0" as the canonical minimum.
            other => Term::NumLit {
                base: *other,
                repr: "0".to_string(),
            },
        },
        Type::Unit => Term::UnitLit,
        Type::Fun(args, ret) => {
            let lam_params: Vec<(String, Type)> = args
                .iter()
                .enumerate()
                .map(|(i, t)| (format!("u{i}"), t.clone()))
                .collect();
            Term::Lambda {
                params: lam_params,
                ret: (**ret).clone(),
                body: Box::new(minimum_for(ret)),
            }
        }
        Type::Product(a, b) => Term::PolyCall {
            name: "__pair__".to_string(),
            type_args: vec![(**a).clone(), (**b).clone()],
            args: vec![minimum_for(a), minimum_for(b)],
        },
        Type::Sum(a, b) => Term::PolyCall {
            name: "__left__".to_string(),
            type_args: vec![(**a).clone(), (**b).clone()],
            args: vec![minimum_for(a)],
        },
        Type::Nominal {
            payload,
            constructor,
            ..
        } => Term::App(
            Box::new(Term::Var(constructor.clone())),
            vec![minimum_for(payload)],
        ),
        Type::Param {
            payload,
            constructor,
            ..
        } => Term::PolyCall {
            name: constructor.clone(),
            type_args: vec![(**payload).clone()],
            args: vec![minimum_for(payload)],
        },
        Type::Label {
            label,
            constructor,
            payload,
            ..
        } => Term::LabelConstruct {
            label: label.clone(),
            constructor: constructor.clone(),
            payload: Box::new(minimum_for(payload)),
        },
    }
}
