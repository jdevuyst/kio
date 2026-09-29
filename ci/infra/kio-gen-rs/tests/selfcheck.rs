//! Self-consistency tests for the generator.
//!
//! For every program emitted by `generate::program_from_seed`, an
//! independent re-walk reproduces the type the generator claimed,
//! confirms every variable reference is in scope, and that the
//! rendered source matches the expected shape. These are the
//! generator's own correctness checks — separate from the
//! differential pass against a Kio implementation.

use std::collections::HashMap;

use kio_gen::ast::{Base, Program, Term, Type, UfcsKind};
use kio_gen::{emit, generate, program_seed, render, shrink};

fn initial_env() -> HashMap<String, Type> {
    // Use the surface-inclusive env so the type-walker accepts any
    // program the generator could have produced (Kio'-only or
    // surface-using). The kio-gen invariant is that surface programs
    // only reference generated label members when they exist; this overapproxi-
    // mates that — bindings present here that aren't actually
    // referenced are harmless.
    generate::wrapper_env(true).into_iter().collect()
}

/// Pin the `Type::Param` payload-equality invariant that
/// `match_disjoint` (slice 2.9) relies on. The fix routes the
/// `match!` production through `pick_match_disjoint_pair`, which in
/// turn checks `a != b` to reject two structurally-identical
/// scrutinee arms (the case where one clause's parameter type
/// covers both DNF branches). For two `Box(I32)` Param
/// instantiations to be rejected, the derived `Eq` has to do a
/// deep payload comparison — which it does, by virtue of
/// `Type::Param`'s derive — but if anyone ever re-derives the
/// trait on a future variant of the AST, this test trips loudly
/// before the unreachable-clause flake re-surfaces.
#[test]
fn type_param_payload_equality_is_deep() {
    use kio_gen::ast::Base;
    let box_i32_a = Type::Param {
        name: "Box".to_string(),
        payload: Box::new(Type::Base(Base::I32)),
        constructor: "box_any".to_string(),
        projector: "unbox_any".to_string(),
    };
    let box_i32_b = box_i32_a.clone();
    assert_eq!(
        box_i32_a, box_i32_b,
        "two structurally identical Box(I32) types must compare equal"
    );

    let box_str = Type::Param {
        name: "Box".to_string(),
        payload: Box::new(Type::Base(Base::Str)),
        constructor: "box_any".to_string(),
        projector: "unbox_any".to_string(),
    };
    assert_ne!(
        box_i32_a, box_str,
        "Box(I32) and Box(String) must compare distinct"
    );
}

const SAMPLE_SEEDS: u64 = 200;

#[test]
fn generated_support_exercises_ordinary_and_marked_type_names() {
    let prog = generate::program_from_seed(program_seed(0xC0FFEE, 0));
    let files = emit::render_package_files(&prog);
    let root = files
        .iter()
        .find(|(path, _)| path == "workdir/prog.kio")
        .map(|(_, source)| source.as_str())
        .expect("generated root module");

    assert!(root.contains("pub newtype I32_box  : I32"), "{root}");
    assert!(root.contains("pub newtype _I32_box : I32"), "{root}");
    assert!(
        root.contains("_I32_box.mk_marked_i32_box(x)"),
        "marked type name must occur at a member-call type head: {root}"
    );
    assert!(
        root.contains("unbox_marked_i32(b: _I32_box) -> I32"),
        "marked type name must occur in signature position: {root}"
    );
}

#[test]
fn programs_are_well_typed() {
    for seed in 0..SAMPLE_SEEDS {
        let prog = generate::program_from_seed(program_seed(0xC0FFEE, seed));
        let mut env: HashMap<String, Type> = initial_env();
        env.extend(prog.params.iter().cloned());
        let inferred = type_of(&prog.body, &env)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}\nbody = {:#?}", prog.body));
        assert_eq!(
            inferred, prog.ret,
            "seed {seed}: body has type {inferred:?} but fn declares {:?}",
            prog.ret
        );
    }
}

#[test]
fn no_seed_panics() {
    for seed in 0..2_000u64 {
        let _ = generate::program_from_seed(seed);
    }
}

#[test]
fn deterministic() {
    for seed in 0..50u64 {
        let p1 = generate::program_from_seed(seed);
        let p2 = generate::program_from_seed(seed);
        assert_eq!(render::render_fn_def(&p1), render::render_fn_def(&p2));
    }
}

#[test]
fn parallel_program_seed_is_deterministic() {
    let pairs: Vec<(u64, u64)> = (0..100u64).map(|i| (program_seed(7, i), i)).collect();
    for (seed, i) in pairs {
        assert_eq!(seed, program_seed(7, i));
    }
}

/// Independent re-walk of the generator's claim: returns the body's
/// type or a free-variable / mismatch error.
fn type_of(t: &Term, env: &HashMap<String, Type>) -> Result<Type, String> {
    match t {
        Term::NumLit { base, .. } => Ok(Type::Base(*base)),
        Term::StrLit(_) => Ok(Type::Base(Base::Str)),
        Term::BoolLit(_) => Ok(Type::Base(Base::Bool)),
        Term::UnitLit => Ok(Type::Unit),
        Term::Var(n) => env.get(n).cloned().ok_or_else(|| format!("free var: {n}")),
        Term::Lambda { params, ret, body } => {
            let mut e2 = env.clone();
            for (n, ty) in params {
                e2.insert(n.clone(), ty.clone());
            }
            let actual = type_of(body, &e2)?;
            if &actual != ret {
                return Err(format!(
                    "lambda body has type {actual:?} != declared {ret:?}"
                ));
            }
            let arg_types: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
            Ok(Type::Fun(arg_types, Box::new(ret.clone())))
        }
        Term::App(f, args) => {
            let f_ty = type_of(f, env)?;
            let Type::Fun(params, ret) = f_ty else {
                return Err(format!("applying non-function: {f_ty:?}"));
            };
            if params.len() != args.len() {
                return Err(format!(
                    "arity mismatch: callee expects {}, got {}",
                    params.len(),
                    args.len()
                ));
            }
            for (i, (p, a)) in params.iter().zip(args.iter()).enumerate() {
                let at = type_of(a, env)?;
                if &at != p {
                    return Err(format!("arg {i}: actual {at:?} != param {p:?}"));
                }
            }
            Ok(*ret)
        }
        Term::Let {
            name,
            ty,
            rhs,
            body,
        } => {
            let rt = type_of(rhs, env)?;
            if &rt != ty {
                return Err(format!(
                    "let `{name}` rhs has type {rt:?} != declared {ty:?}"
                ));
            }
            let mut e2 = env.clone();
            e2.insert(name.clone(), ty.clone());
            type_of(body, &e2)
        }
        Term::PolyCall {
            name,
            type_args,
            args,
        } => match name.as_str() {
            "id" => {
                if type_args.len() != 1 || args.len() != 1 {
                    return Err(format!(
                        "id: expected 1 type arg + 1 value arg, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let t = &type_args[0];
                let at = type_of(&args[0], env)?;
                if &at != t {
                    return Err(format!("id: arg type {at:?} != type arg {t:?}"));
                }
                Ok(t.clone())
            }
            "__if_then_else__" => {
                if type_args.len() != 1 || args.len() != 3 {
                    return Err(format!(
                        "__if_then_else__: expected 1 type arg + 3 value args, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let t = &type_args[0];
                let c_ty = type_of(&args[0], env)?;
                if c_ty != Type::Base(Base::Bool) {
                    return Err(format!("__if_then_else__: c has type {c_ty:?}, want Bool"));
                }
                let expected_arm = Type::Fun(vec![], Box::new(t.clone()));
                let then_ty = type_of(&args[1], env)?;
                if then_ty != expected_arm {
                    return Err(format!(
                        "__if_then_else__: then arm has type {then_ty:?}, want {expected_arm:?}"
                    ));
                }
                let else_ty = type_of(&args[2], env)?;
                if else_ty != expected_arm {
                    return Err(format!(
                        "__if_then_else__: else arm has type {else_ty:?}, want {expected_arm:?}"
                    ));
                }
                Ok(t.clone())
            }
            "__pair__" => {
                if type_args.len() != 2 || args.len() != 2 {
                    return Err(format!(
                        "__pair__: expected 2 type args + 2 value args, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let xt = type_of(&args[0], env)?;
                if &xt != a {
                    return Err(format!("__pair__: x type {xt:?} != type arg {a:?}"));
                }
                let yt = type_of(&args[1], env)?;
                if &yt != b {
                    return Err(format!("__pair__: y type {yt:?} != type arg {b:?}"));
                }
                Ok(Type::Product(Box::new(a.clone()), Box::new(b.clone())))
            }
            "__fst__" => {
                if type_args.len() != 2 || args.len() != 1 {
                    return Err(format!(
                        "__fst__: expected 2 type args + 1 value arg, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let pt = type_of(&args[0], env)?;
                let expected = Type::Product(Box::new(a.clone()), Box::new(b.clone()));
                if pt != expected {
                    return Err(format!("__fst__: p type {pt:?} != {expected:?}"));
                }
                Ok(a.clone())
            }
            "__snd__" => {
                if type_args.len() != 2 || args.len() != 1 {
                    return Err(format!(
                        "__snd__: expected 2 type args + 1 value arg, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let pt = type_of(&args[0], env)?;
                let expected = Type::Product(Box::new(a.clone()), Box::new(b.clone()));
                if pt != expected {
                    return Err(format!("__snd__: p type {pt:?} != {expected:?}"));
                }
                Ok(b.clone())
            }
            "__left__" => {
                if type_args.len() != 2 || args.len() != 1 {
                    return Err(format!(
                        "__left__: expected 2 type args + 1 value arg, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let xt = type_of(&args[0], env)?;
                if &xt != a {
                    return Err(format!("__left__: x type {xt:?} != {a:?}"));
                }
                Ok(Type::Sum(Box::new(a.clone()), Box::new(b.clone())))
            }
            "__right__" => {
                if type_args.len() != 2 || args.len() != 1 {
                    return Err(format!(
                        "__right__: expected 2 type args + 1 value arg, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let xt = type_of(&args[0], env)?;
                if &xt != b {
                    return Err(format!("__right__: x type {xt:?} != {b:?}"));
                }
                Ok(Type::Sum(Box::new(a.clone()), Box::new(b.clone())))
            }
            "const1" => {
                if type_args.len() != 2 || args.len() != 2 {
                    return Err(format!(
                        "const1: expected 2 type args + 2 value args, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let xt = type_of(&args[0], env)?;
                if &xt != a {
                    return Err(format!("const1: x type {xt:?} != {a:?}"));
                }
                let yt = type_of(&args[1], env)?;
                if &yt != b {
                    return Err(format!("const1: y type {yt:?} != {b:?}"));
                }
                Ok(a.clone())
            }
            "const2" => {
                if type_args.len() != 2 || args.len() != 2 {
                    return Err(format!(
                        "const2: expected 2 type args + 2 value args, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let xt = type_of(&args[0], env)?;
                if &xt != a {
                    return Err(format!("const2: x type {xt:?} != {a:?}"));
                }
                let yt = type_of(&args[1], env)?;
                if &yt != b {
                    return Err(format!("const2: y type {yt:?} != {b:?}"));
                }
                Ok(b.clone())
            }
            "__either__" => {
                if type_args.len() != 3 || args.len() != 3 {
                    return Err(format!(
                        "__either__: expected 3 type args + 3 value args, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let b = &type_args[1];
                let c = &type_args[2];
                let st = type_of(&args[0], env)?;
                let expected_s = Type::Sum(Box::new(a.clone()), Box::new(b.clone()));
                if st != expected_s {
                    return Err(format!("__either__: s type {st:?} != {expected_s:?}"));
                }
                let fl_ty = type_of(&args[1], env)?;
                let expected_fl = Type::Fun(vec![a.clone()], Box::new(c.clone()));
                if fl_ty != expected_fl {
                    return Err(format!("__either__: fl type {fl_ty:?} != {expected_fl:?}"));
                }
                let fr_ty = type_of(&args[2], env)?;
                let expected_fr = Type::Fun(vec![b.clone()], Box::new(c.clone()));
                if fr_ty != expected_fr {
                    return Err(format!("__either__: fr type {fr_ty:?} != {expected_fr:?}"));
                }
                Ok(c.clone())
            }
            "box_any" => {
                if type_args.len() != 1 || args.len() != 1 {
                    return Err(format!(
                        "box_any: expected 1 type arg + 1 value arg, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let xt = type_of(&args[0], env)?;
                if xt != *a {
                    return Err(format!("box_any: x type {xt:?} != {a:?}"));
                }
                Ok(Type::Param {
                    name: "Box".to_string(),
                    payload: Box::new(a.clone()),
                    constructor: "box_any".to_string(),
                    projector: "unbox_any".to_string(),
                })
            }
            "unbox_any" => {
                if type_args.len() != 1 || args.len() != 1 {
                    return Err(format!(
                        "unbox_any: expected 1 type arg + 1 value arg, got {} + {}",
                        type_args.len(),
                        args.len()
                    ));
                }
                let a = &type_args[0];
                let expected_box = Type::Param {
                    name: "Box".to_string(),
                    payload: Box::new(a.clone()),
                    constructor: "box_any".to_string(),
                    projector: "unbox_any".to_string(),
                };
                let xt = type_of(&args[0], env)?;
                if xt != expected_box {
                    return Err(format!("unbox_any: x type {xt:?} != {expected_box:?}"));
                }
                Ok(a.clone())
            }
            other => Err(format!("unknown poly helper: {other}")),
        },
        Term::TypeMember { .. } => {
            // The generator no longer emits TypeMember — kio-rs
            // doesn't yet support cross-module dotted-path access, so
            // every constructor / projector goes through a generated
            // wrapper function reached via Term::App + Term::Var.
            unreachable!("TypeMember not produced by the current generator")
        }
        Term::LabelConstruct {
            label,
            constructor,
            payload,
        } => {
            // Look up the label's expected payload type by walking the
            // label catalog. The constructor member's signature is
            // `payload -> LabelType`; we pull both ends from there.
            use kio_gen::generate;
            let label_ty = generate::label_catalog()
                .into_iter()
                .find(|t| matches!(t, Type::Label { label: name, .. } if name == label))
                .ok_or_else(|| format!("unknown label: {label}"))?;
            let Type::Label {
                payload: ref p,
                constructor: ref c,
                ..
            } = label_ty
            else {
                return Err(format!(
                    "label_catalog entry for {label} is not Type::Label"
                ));
            };
            if c != constructor {
                return Err(format!(
                    "LabelConstruct: declared ctor `{constructor}` doesn't match catalog `{c}`"
                ));
            }
            let pt = type_of(payload, env)?;
            if pt != **p {
                return Err(format!(
                    "LabelConstruct({label}): payload {pt:?} != expected {p:?}"
                ));
            }
            Ok(label_ty)
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
        | Term::Fit { inner } => {
            // Identity-only coercions: the inner's type is the
            // result type. The actual coercion semantics live in
            // kio-rs's elaborator — selfcheck only verifies the
            // generator stays inside the identity envelope.
            type_of(inner, env)
        }
        Term::Ufcs {
            receiver,
            callee_name,
            rest_args,
            callee_kind,
        } => {
            // UFCS is a rendering rewrap around an underlying call.
            // Selfcheck retypes through the recorded fallback
            // shape: a `PolyCall` with `(type_args, receiver,
            // rest_args)` or an `App` with `(receiver, rest_args)`.
            let mut args = vec![(**receiver).clone()];
            args.extend(rest_args.iter().cloned());
            let reconstructed = match callee_kind {
                UfcsKind::PolyCall { type_args } => Term::PolyCall {
                    name: callee_name.clone(),
                    type_args: type_args.clone(),
                    args,
                },
                UfcsKind::App => Term::App(Box::new(Term::Var(callee_name.clone())), args),
            };
            type_of(&reconstructed, env)
        }
        Term::FnPlaceholder { params, ret, body } => {
            // Same shape as a regular Lambda for typing purposes:
            // the body is checked under params, and the result is a
            // function type from params to body.
            let mut e2 = env.clone();
            for (n, ty) in params {
                e2.insert(n.clone(), ty.clone());
            }
            let actual = type_of(body, &e2)?;
            if &actual != ret {
                return Err(format!(
                    ".arg. body has type {actual:?} != declared {ret:?}"
                ));
            }
            let arg_types: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
            Ok(Type::Fun(arg_types, Box::new(ret.clone())))
        }
        Term::Placeholder {
            fallback_name, ty, ..
        } => {
            // Inside a FnPlaceholder body, the placeholder is
            // pre-resolved to the param it stands for. The env
            // entry was added by the enclosing FnPlaceholder's
            // recursion above.
            match env.get(fallback_name) {
                Some(t) if t == ty => Ok(t.clone()),
                Some(t) => Err(format!(
                    "Placeholder {fallback_name}: declared {ty:?} != env {t:?}"
                )),
                None => Err(format!("Placeholder {fallback_name}: not in scope")),
            }
        }
        Term::LiteralAliasRef { name: _, literal } => {
            // Literal alias: at use sites the bound literal is
            // substituted in. Selfcheck collapses to the bound
            // literal's type.
            type_of(literal, env)
        }
        Term::OpCall {
            op_tokens: _,
            callee,
            lhs,
            rhs,
        } => {
            // Op-call elaborates to `callee(lhs, rhs)` — re-type
            // through the same App path. The callee is bound in the
            // env by the generator (the op declaration goes through
            // the same generated-helper path as ordinary fn declarations).
            let f_ty = env
                .get(callee)
                .ok_or_else(|| format!("op callee not in scope: {callee}"))?
                .clone();
            let Type::Fun(params, ret) = f_ty else {
                return Err(format!("op callee not a function: {callee}"));
            };
            if params.len() != 2 {
                return Err(format!("op callee {callee} arity {} != 2", params.len()));
            }
            let lt = type_of(lhs, env)?;
            if lt != params[0] {
                return Err(format!("op lhs: {lt:?} != {:?}", params[0]));
            }
            let rt = type_of(rhs, env)?;
            if rt != params[1] {
                return Err(format!("op rhs: {rt:?} != {:?}", params[1]));
            }
            Ok(*ret)
        }
        Term::UserElaborator { name: _, inner } => {
            // Generated user elaborators are identity-style
            // reflected-ABI templates; their Kio' fallback is the
            // wrapped term.
            type_of(inner, env)
        }
        Term::MatchBang {
            scrutinee,
            left_param,
            left_ty,
            left_body,
            right_param,
            right_ty,
            right_body,
            ret_ty,
        } => {
            // Two-clause `match!` against a sum-typed scrutinee.
            // Per spec, both clause bodies produce the same common
            // result type `ret_ty`.
            let st = type_of(scrutinee, env)?;
            let expected_sum = Type::Sum(Box::new(left_ty.clone()), Box::new(right_ty.clone()));
            if st != expected_sum {
                return Err(format!(
                    "match!: scrutinee {st:?} != expected {expected_sum:?}"
                ));
            }
            let mut le = env.clone();
            le.insert(left_param.clone(), left_ty.clone());
            let lt = type_of(left_body, &le)?;
            if lt != *ret_ty {
                return Err(format!("match!: left body {lt:?} != ret_ty {ret_ty:?}"));
            }
            let mut re = env.clone();
            re.insert(right_param.clone(), right_ty.clone());
            let rt = type_of(right_body, &re)?;
            if rt != *ret_ty {
                return Err(format!("match!: right body {rt:?} != ret_ty {ret_ty:?}"));
            }
            Ok(ret_ty.clone())
        }
        Term::IfElse {
            cond,
            then,
            else_,
            then_ty,
            else_ty,
        } => {
            let ct = type_of(cond, env)?;
            if ct != Type::bool() {
                return Err(format!("if-cond: type {ct:?} != Bool"));
            }
            let tt = type_of(then, env)?;
            if tt != *then_ty {
                return Err(format!("if-then: declared {then_ty:?} != actual {tt:?}"));
            }
            let et = type_of(else_, env)?;
            if et != *else_ty {
                return Err(format!("if-else: declared {else_ty:?} != actual {et:?}"));
            }
            if then_ty != else_ty {
                return Err(format!(
                    "if: branch types differ: then {then_ty:?}, else {else_ty:?}"
                ));
            }
            Ok(then_ty.clone())
        }
    }
}

#[test]
fn rendered_fn_def_matches_shape() {
    // Spot-check the rendered Kio across many seeds: starts with
    // `fn `, contains the declared return type rendered through
    // the same surface-mode pipeline `render_fn_def` uses, is non-empty.
    for seed in 0..100u64 {
        let prog: Program = generate::program_from_seed(seed);
        let s = render::render_fn_def(&prog);
        assert!(s.starts_with("fn "), "seed {seed}: render: {s}");
        let opts = render::RenderOpts {
            surface: prog.uses_surface,
        };
        let ret = render::render_type_opts(&prog.ret, &opts);
        assert!(s.contains(&format!("-> {ret}")), "seed {seed}: render: {s}");
        assert!(
            s.contains('{') && s.contains('}'),
            "seed {seed}: render: {s}"
        );
    }
}

#[test]
fn shrunk_programs_are_well_typed() {
    // Whatever the generator produced, the minimised body must
    // still match the declared return type — the mutator depends on
    // the wrapping construct, not on the body's structure, so this
    // is the contract `shrink::minimize_body` has to keep.
    for seed in 0..200u64 {
        let prog = generate::program_from_seed(seed);
        let shrunk = shrink::minimize_body(&prog);
        let mut env: HashMap<String, Type> = initial_env();
        env.extend(shrunk.params.iter().cloned());
        let inferred = type_of(&shrunk.body, &env)
            .unwrap_or_else(|e| panic!("seed {seed}: shrunk type_of: {e}"));
        assert_eq!(
            inferred, shrunk.ret,
            "seed {seed}: shrunk body has type {inferred:?} != declared {:?}",
            shrunk.ret
        );
    }
}

#[test]
fn each_base_type_is_eventually_emitted() {
    // Sanity-check that the base-type picker isn't stuck on a tiny
    // subset. Layer (2) tightens this against the full corpus; here
    // we just confirm every base shows up as a return type within a
    // reasonable seed budget.
    use std::collections::HashSet;
    let mut seen: HashSet<Base> = HashSet::new();
    for seed in 0..2_000u64 {
        if let Type::Base(b) = generate::program_from_seed(seed).ret {
            seen.insert(b);
        }
    }
    for b in Base::all() {
        assert!(seen.contains(b), "base {b:?} never returned in 2_000 seeds");
    }
}
