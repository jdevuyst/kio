//! Render the generator's typed AST to Kio source text.
//!
//! `RenderOpts::surface` controls whether surface forms are
//! recognised: with `surface = false` (the default and the rendering
//! used for Kio'-only programs), every term renders to its Kio'
//! form; with `surface = true`, surface-form rewrites fire (tuple
//! literals, `{label = e}`, `match!`, spine elaborators, user
//! elaborators, `if!`/`else`).
//! `Program::uses_surface` decides which opts to use for a given
//! program: surface-using programs render with the flag enabled.

use std::collections::HashSet;

use kio_gen::ast::{Base, Program, Term, Type, UfcsKind};

const MATCH_SOURCE_BINDER_PREFIX: &str = "_gen_match_source";

#[derive(Debug, Default)]
struct RenderContext {
    used_value_names: HashSet<String>,
    next_match_source: usize,
}

impl RenderContext {
    fn for_term(term: &Term, opts: &RenderOpts) -> Self {
        let mut context = Self::default();
        if opts.surface {
            collect_emitted_value_names(term, opts, &mut context.used_value_names);
        }
        context
    }

    fn for_program(program: &Program, opts: &RenderOpts) -> Self {
        let mut context = Self::for_term(&program.body, opts);
        if opts.surface {
            context.used_value_names.insert(program.fn_def_name.clone());
            for (name, ty) in &program.params {
                context.used_value_names.insert(name.clone());
                collect_surface_as_pattern_names(name, ty, opts, &mut context.used_value_names);
            }
        }
        context
    }

    fn allocate_match_source(&mut self) -> String {
        loop {
            let candidate = format!("{MATCH_SOURCE_BINDER_PREFIX}{}", self.next_match_source);
            self.next_match_source += 1;
            if self.used_value_names.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

/// Render-time switches. Default = render every term to its Kio'
/// form. Set `surface = true` to enable every surface-form rewrite
/// the renderer knows about: tuple literals, `{label = e}`, `match!`,
/// spine elaborators, user elaborators, `if!`/`else`. Programs whose
/// `uses_surface` is true are rendered with this flag on.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderOpts {
    /// Render surface-form constructs as their surface spelling.
    /// When false, every construct renders to its Kio' equivalent
    /// (tuple-literal chains stay as nested `__pair__`, `IfElse`
    /// nodes elaborate to `__if_then_else__(R, cond, .() { … },
    /// .() { … })`).
    pub surface: bool,
}

fn collect_surface_as_pattern_names(
    name: &str,
    ty: &Type,
    opts: &RenderOpts,
    names: &mut HashSet<String>,
) {
    if opts.surface && matches!(ty, Type::Product(_, _)) {
        names.insert(format!("_gen_{name}_l"));
        names.insert(format!("_gen_{name}_r"));
    }
}

/// Seed the renderer-owned allocator from every value identity that
/// can occur in this lexical owner. The walk includes nested binders
/// and free call heads because the `do` expression introduced around
/// a match source encloses both its source and its clause bodies.
fn collect_emitted_value_names(term: &Term, opts: &RenderOpts, names: &mut HashSet<String>) {
    match term {
        Term::NumLit { .. } | Term::StrLit(_) | Term::BoolLit(_) | Term::UnitLit => {}
        Term::Var(name) => {
            names.insert(name.clone());
        }
        Term::Lambda { params, body, .. } => {
            for (name, ty) in params {
                names.insert(name.clone());
                collect_surface_as_pattern_names(name, ty, opts, names);
            }
            collect_emitted_value_names(body, opts, names);
        }
        Term::App(function, args) => {
            collect_emitted_value_names(function, opts, names);
            for arg in args {
                collect_emitted_value_names(arg, opts, names);
            }
        }
        Term::Let {
            name, rhs, body, ..
        } => {
            names.insert(name.clone());
            collect_emitted_value_names(rhs, opts, names);
            collect_emitted_value_names(body, opts, names);
        }
        Term::PolyCall { name, args, .. } => {
            names.insert(name.clone());
            for arg in args {
                collect_emitted_value_names(arg, opts, names);
            }
        }
        Term::TypeMember { args, .. } => {
            for arg in args {
                collect_emitted_value_names(arg, opts, names);
            }
        }
        Term::IfElse {
            cond, then, else_, ..
        } => {
            collect_emitted_value_names(cond, opts, names);
            collect_emitted_value_names(then, opts, names);
            collect_emitted_value_names(else_, opts, names);
        }
        Term::LabelConstruct {
            constructor,
            payload,
            ..
        } => {
            if !opts.surface {
                names.insert(constructor.clone());
            }
            collect_emitted_value_names(payload, opts, names);
        }
        Term::MatchBang {
            scrutinee,
            left_param,
            left_body,
            right_param,
            right_body,
            ..
        } => {
            names.insert(left_param.clone());
            names.insert(right_param.clone());
            collect_emitted_value_names(scrutinee, opts, names);
            collect_emitted_value_names(left_body, opts, names);
            collect_emitted_value_names(right_body, opts, names);
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
        | Term::Fit { inner } => collect_emitted_value_names(inner, opts, names),
        Term::Ufcs {
            receiver,
            callee_name,
            rest_args,
            ..
        } => {
            names.insert(callee_name.clone());
            collect_emitted_value_names(receiver, opts, names);
            for arg in rest_args {
                collect_emitted_value_names(arg, opts, names);
            }
        }
        Term::FnPlaceholder { params, body, .. } => {
            if !opts.surface {
                for (name, _) in params {
                    names.insert(name.clone());
                }
            }
            collect_emitted_value_names(body, opts, names);
        }
        Term::Placeholder { fallback_name, .. } => {
            if !opts.surface {
                names.insert(fallback_name.clone());
            }
        }
        Term::LiteralAliasRef { name, literal } => {
            if opts.surface {
                names.insert(name.clone());
            }
            collect_emitted_value_names(literal, opts, names);
        }
        Term::OpCall {
            callee, lhs, rhs, ..
        } => {
            if !opts.surface {
                names.insert(callee.clone());
            }
            collect_emitted_value_names(lhs, opts, names);
            collect_emitted_value_names(rhs, opts, names);
        }
        Term::UserElaborator { name, inner } => {
            if opts.surface {
                names.insert(name.clone());
            }
            collect_emitted_value_names(inner, opts, names);
        }
    }
}

pub fn render_base(b: Base) -> &'static str {
    b.type_name()
}

pub fn render_type(t: &Type) -> String {
    if let Some(alias) = lookup_alias(t) {
        return alias.to_string();
    }
    match t {
        Type::Base(b) => render_base(*b).to_string(),
        Type::Unit => ".".to_string(),
        Type::Fun(args, ret) => {
            let ret = render_type(ret);
            let domain = product_domain(args);
            format!("{} -> {ret}", render_function_domain(&domain))
        }
        // `&` and `|` are always parenthesized in Kio'. Function-typed
        // operands need their own outer parens so the chain boundary
        // stays explicit.
        Type::Product(a, b) => format!("({} & {})", paren_if_function(a), paren_if_function(b)),
        Type::Sum(a, b) => format!("({} | {})", paren_if_function(a), paren_if_function(b)),
        Type::Nominal { name, .. } => name.clone(),
        Type::Param { name, payload, .. } => format!("{name}({})", render_type(payload)),
        Type::Label { newtype_name, .. } => newtype_name.clone(),
    }
}

/// Type rendering with surface-form recognition. Today every type
/// variant renders the same in surface and Kio' mode: generated
/// label types are written by their nominal type names in every type
/// position, while value-position surface forms (`{<label> = e}`,
/// `match!`, etc.) can still fire in the body.
pub fn render_type_opts(t: &Type, _opts: &RenderOpts) -> String {
    render_type(t)
}

fn render_function_domain(t: &Type) -> String {
    match t {
        Type::Product(_, _) | Type::Sum(_, _) => render_type(t),
        Type::Fun(_, _) => format!("({})", render_type(t)),
        _ => render_type(t),
    }
}

fn product_domain(args: &[Type]) -> Type {
    match args {
        [] => Type::Unit,
        [single] => single.clone(),
        [head, tail @ ..] => Type::Product(Box::new(head.clone()), Box::new(product_domain(tail))),
    }
}

/// If `t` is structurally identical to a `pub type` declared in
/// the generated root module, return the alias's source name. The kio-rs
/// typechecker unfolds type aliases structurally, so emitting the
/// alias name in source position is a valid Kio' substitute that
/// drives the type-unfolding path in the implementation.
fn lookup_alias(t: &Type) -> Option<&'static str> {
    let intpair = Type::Product(Box::new(Type::i32()), Box::new(Type::i32()));
    let optint = Type::Sum(Box::new(Type::Unit), Box::new(Type::i32()));
    if *t == intpair {
        Some("Intpair")
    } else if *t == optint {
        Some("Optint")
    } else {
        None
    }
}

fn paren_if_function(t: &Type) -> String {
    if matches!(t, Type::Fun(_, _)) {
        format!("({})", render_type(t))
    } else {
        render_type(t)
    }
}

pub fn render_term(t: &Term) -> String {
    render_term_opts(t, &RenderOpts::default())
}

/// Like [`render_term_opts`] but for a position where statements
/// are admissible (a block body). `Term::Let` renders as a
/// straight `let X = E; rest` chain; other terms render the same
/// as [`render_term_opts`].
pub fn render_block_body_opts(t: &Term, opts: &RenderOpts) -> String {
    let mut context = RenderContext::for_term(t, opts);
    render_block_body_opts_with_context(t, opts, &mut context)
}

fn render_block_body_opts_with_context(
    t: &Term,
    opts: &RenderOpts,
    context: &mut RenderContext,
) -> String {
    if opts.surface
        && let Some(items) = collect_tuple_items(t)
    {
        let parts: Vec<String> = items
            .iter()
            .map(|i| render_term_opts_with_context(i, opts, context))
            .collect();
        return format!("({})", parts.join(", "));
    }
    if let Term::Let {
        name,
        ty,
        rhs,
        body,
    } = t
    {
        let _ = ty;
        // RHS is rendered as a plain expression (lets in expr
        // position get wrapped in IIFEs via render_term_opts).
        // Body stays in block-statement position.
        return format!(
            "let {name} = {}; {}",
            render_term_opts_with_context(rhs, opts, context),
            render_block_body_opts_with_context(body, opts, context)
        );
    }
    render_term_opts_with_context(t, opts, context)
}

pub fn render_term_opts(t: &Term, opts: &RenderOpts) -> String {
    let mut context = RenderContext::for_term(t, opts);
    render_term_opts_with_context(t, opts, &mut context)
}

fn render_term_opts_with_context(
    t: &Term,
    opts: &RenderOpts,
    context: &mut RenderContext,
) -> String {
    if opts.surface
        && let Some(items) = collect_tuple_items(t)
    {
        let parts: Vec<String> = items
            .iter()
            .map(|i| render_term_opts_with_context(i, opts, context))
            .collect();
        return format!("({})", parts.join(", "));
    }
    match t {
        // The explicit `(Type)` annotation form (specs/language.md
        // § Literals). The generated package declares every base as a
        // distinct role-bearing host type, so a bare
        // literal has no unique tier-3 candidate — the annotation pins
        // it.
        Term::NumLit { base, repr } => format!("{repr}({})", base.type_name()),
        Term::StrLit(s) => format!("{}(String)", render_str_lit(s)),
        Term::BoolLit(b) => format!("{}(Bool)", if *b { ".t" } else { ".f" }),
        Term::UnitLit => "()".to_string(),
        Term::Var(n) => n.clone(),
        Term::Lambda { params, ret, body } => {
            // `fn` value parameters are un-annotated and there is
            // no return-type clause; the parameter and return
            // types come from the call-site context. The body is
            // a block, so any leading `Term::Let`s render as
            // statements rather than expression-position IIFEs.
            //
            // **Surface-mode as-pattern injection.** When a
            // parameter's type is a `Product`, in surface mode the
            // renderer wraps the binder as the as-pattern form
            // `name: (_gen_<n>_l: L, _gen_<n>_r: R)` — the
            // destructured-component names use a per-binder unique
            // spelling (drawn from the binder's own name) so they
            // round-trip through `kio fmt`. This exercises the
            // parameter-pattern surface form on every generated
            // product-typed lambda parameter; the body still
            // references the original `name`, so the destructured
            // components are bound-but-unused — the typer accepts
            // that under the existing dead-binding rule. In Kio'
            // mode (where the surface form isn't admissible) the
            // binder reverts to the bare name.
            let _ = ret;
            let p: Vec<String> = params
                .iter()
                .map(|(n, t)| {
                    if opts.surface
                        && let Type::Product(l, r) = t
                    {
                        format!(
                            "{n}: (_gen_{n}_l: {}, _gen_{n}_r: {})",
                            render_type_opts(l, opts),
                            render_type_opts(r, opts),
                        )
                    } else {
                        n.clone()
                    }
                })
                .collect();
            format!(
                ".({}) {{ {} }}",
                p.join(", "),
                render_block_body_opts_with_context(body, opts, context)
            )
        }
        Term::App(f, args) => {
            let a: Vec<String> = args
                .iter()
                .map(|x| render_term_opts_with_context(x, opts, context))
                .collect();
            // A bare lambda in callee position would parse ambiguously; bind it
            // through a `let` instead of applying it inline. Generators only
            // produce App with a Var callee, so this branch is just defensive.
            let f_s = match **f {
                Term::Lambda { .. } | Term::Let { .. } => {
                    format!("({})", render_term_opts_with_context(f, opts, context))
                }
                _ => render_term_opts_with_context(f, opts, context),
            };
            format!("{f_s}({})", a.join(", "))
        }
        Term::Let { .. } => {
            // `Term::Let` in expression position. Wrap as an IIFE
            // — `.() { let X = E; body }()` — since `let` is a
            // block statement, not an expression, post-`let-in`
            // removal. The block-body rendering inside the IIFE
            // handles further nested lets through the
            // [`render_block_body_opts`] entrypoint.
            format!(
                ".() {{ {} }}()",
                render_block_body_opts_with_context(t, opts, context)
            )
        }
        Term::PolyCall {
            name,
            type_args,
            args,
        } => {
            let mut parts: Vec<String> = type_args
                .iter()
                .map(|ty| render_type_opts(ty, opts))
                .collect();
            parts.extend(
                args.iter()
                    .map(|x| render_term_opts_with_context(x, opts, context)),
            );
            format!("{name}({})", parts.join(", "))
        }
        Term::TypeMember {
            type_name,
            member,
            args,
        } => {
            let parts: Vec<String> = args
                .iter()
                .map(|x| render_term_opts_with_context(x, opts, context))
                .collect();
            format!("{type_name}.{member}({})", parts.join(", "))
        }
        Term::LabelConstruct {
            label,
            constructor,
            payload,
        } => {
            if opts.surface {
                format!(
                    "{{{label} = {}}}",
                    render_term_opts_with_context(payload, opts, context)
                )
            } else {
                format!(
                    "{constructor}({})",
                    render_term_opts_with_context(payload, opts, context)
                )
            }
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
            // The MatchBang AST stores clause bodies that already
            // produce the common `ret_ty`. Surface mode emits them
            // directly inside `match!` clauses; Kio' mode emits the
            // equivalent homogeneous `__either__` arms.
            if opts.surface {
                // A computed match source needs the same ordinary
                // lexical inference boundary a user would write.
                // Allocate before rendering the source so nested
                // matches receive later, distinct names; direct
                // variables already provide the boundary unchanged.
                let source_name = (!matches!(scrutinee.as_ref(), Term::Var(_)))
                    .then(|| context.allocate_match_source());
                let rendered_scrutinee = render_term_opts_with_context(scrutinee, opts, context);
                let rendered_left = render_term_opts_with_context(left_body, opts, context);
                let rendered_right = render_term_opts_with_context(right_body, opts, context);
                let rendered_match = format!(
                    "match!({}) {{ (.({left_param}: {}) {{ {} }}, .({right_param}: {}) {{ {} }}) }}",
                    source_name.as_deref().unwrap_or(&rendered_scrutinee),
                    render_type_opts(left_ty, opts),
                    rendered_left,
                    render_type_opts(right_ty, opts),
                    rendered_right,
                );
                if let Some(source_name) = source_name {
                    format!(
                        "scope! {{ let {source_name} = {rendered_scrutinee}; {rendered_match} }}"
                    )
                } else {
                    rendered_match
                }
            } else {
                format!(
                    "__either__({}, {}, {}, {}, .({left_param}) {{ {} }}, .({right_param}) {{ {} }})",
                    render_type_opts(left_ty, opts),
                    render_type_opts(right_ty, opts),
                    render_type_opts(ret_ty, opts),
                    render_term_opts_with_context(scrutinee, opts, context),
                    render_term_opts_with_context(left_body, opts, context),
                    render_term_opts_with_context(right_body, opts, context),
                )
            }
        }
        Term::Into { inner } => {
            if opts.surface {
                format!(
                    "into!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::Onto { inner } => {
            if opts.surface {
                format!(
                    "onto!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::Iso { inner } => {
            if opts.surface {
                format!(
                    "iso!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::Align { inner } => {
            if opts.surface {
                format!(
                    "align!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::Ease { inner } => {
            if opts.surface {
                format!(
                    "ease!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::Atom { inner } => {
            if opts.surface {
                format!(
                    "atom!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::ReorderSum { inner } => {
            if opts.surface {
                format!(
                    "reorder_sum!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::ReorderProd { inner } => {
            if opts.surface {
                format!(
                    "reorder_prod!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::NarrowSum { inner } => {
            if opts.surface {
                format!(
                    "narrow_sum!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::NarrowProd { inner } => {
            if opts.surface {
                format!(
                    "narrow_prod!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::WidenSum { inner } => {
            if opts.surface {
                format!(
                    "widen_sum!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::WidenProd { inner } => {
            if opts.surface {
                format!(
                    "widen_prod!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::FlattenSum { inner } => {
            if opts.surface {
                format!(
                    "flatten_sum!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::FlattenProd { inner } => {
            if opts.surface {
                format!(
                    "flatten_prod!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::OneSum { inner } => {
            if opts.surface {
                format!(
                    "one_sum!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::OneProd { inner } => {
            if opts.surface {
                format!(
                    "one_prod!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::Fit { inner } => {
            if opts.surface {
                format!(
                    "fit!({})",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::Ufcs {
            receiver,
            callee_name,
            rest_args,
            callee_kind,
        } => {
            if opts.surface {
                // `r.>m` when no further args; `r.>m(args)` when
                // there are more. UFCS does not accept user-supplied
                // type-args at the surface (the spec is value-args
                // only; type-args are backsolved from the receiver
                // and value-args), so the trailing parens hold only
                // value-args.
                if rest_args.is_empty() {
                    format!(
                        "{}.>{callee_name}",
                        render_term_opts_with_context(receiver, opts, context)
                    )
                } else {
                    let rest: Vec<String> = rest_args
                        .iter()
                        .map(|x| render_term_opts_with_context(x, opts, context))
                        .collect();
                    format!(
                        "{}.>{callee_name}({})",
                        render_term_opts_with_context(receiver, opts, context),
                        rest.join(", ")
                    )
                }
            } else {
                match callee_kind {
                    UfcsKind::PolyCall { type_args } => {
                        let mut parts: Vec<String> = type_args
                            .iter()
                            .map(|ty| render_type_opts(ty, opts))
                            .collect();
                        parts.push(render_term_opts_with_context(receiver, opts, context));
                        parts.extend(
                            rest_args
                                .iter()
                                .map(|x| render_term_opts_with_context(x, opts, context)),
                        );
                        format!("{callee_name}({})", parts.join(", "))
                    }
                    UfcsKind::App => {
                        let mut parts: Vec<String> =
                            vec![render_term_opts_with_context(receiver, opts, context)];
                        parts.extend(
                            rest_args
                                .iter()
                                .map(|x| render_term_opts_with_context(x, opts, context)),
                        );
                        format!("{callee_name}({})", parts.join(", "))
                    }
                }
            }
        }
        Term::FnPlaceholder { params, ret, body } => {
            let _ = ret;
            if opts.surface {
                // `.arg. { body }` with `argN` references in the body.
                // Param names are not written at the surface — the
                // fixed `arg` stem sets the implicit naming.
                format!(
                    ".arg. {{ {} }}",
                    render_block_body_opts_with_context(body, opts, context)
                )
            } else {
                // Kio' fallback: a regular `.(<param1>, ..., <paramN>)
                // { body[argN ↦ <paramN>] }` lambda — the same shape
                // the surface desugar pass produces (per
                // `specs/language.md` § Placeholder lambdas). Param
                // names are un-annotated; the call-site context
                // pins the types.
                let p: Vec<String> = params.iter().map(|(n, _t)| n.clone()).collect();
                format!(
                    ".({}) {{ {} }}",
                    p.join(", "),
                    render_block_body_opts_with_context(body, opts, context)
                )
            }
        }
        Term::Placeholder {
            index,
            fallback_name,
            ty: _,
        } => {
            if opts.surface {
                format!("arg{index}")
            } else {
                fallback_name.clone()
            }
        }
        Term::LiteralAliasRef { name, literal } => {
            if opts.surface {
                format!("{name}({})", literal_annotation(literal))
            } else {
                render_term_opts_with_context(literal, opts, context)
            }
        }
        Term::OpCall {
            op_tokens,
            callee,
            lhs,
            rhs,
        } => {
            if opts.surface {
                // Parenthesize the rendered chain unconditionally:
                // the `<+>` op is non-associative (declared as
                // `_ <+> _` in the generated root module), so any nesting of
                // op-calls — `a <+> b <+> c` — has to come with
                // explicit grouping parens. Wrapping the whole
                // OpCall in parens at every render site makes
                // nested chains unambiguous to the parser; wrapping
                // each operand keeps expression forms like lambda
                // calls from being misread as a one-operand operator
                // chain.
                format!(
                    "(({}) {op_tokens} ({}))",
                    render_term_opts_with_context(lhs, opts, context),
                    render_term_opts_with_context(rhs, opts, context)
                )
            } else {
                format!(
                    "{callee}({}, {})",
                    render_term_opts_with_context(lhs, opts, context),
                    render_term_opts_with_context(rhs, opts, context)
                )
            }
        }
        Term::UserElaborator { name, inner } => {
            if opts.surface {
                format!(
                    "{name}!({}, _)",
                    render_term_opts_with_context(inner, opts, context)
                )
            } else {
                render_term_opts_with_context(inner, opts, context)
            }
        }
        Term::IfElse {
            cond,
            then,
            else_,
            then_ty,
            else_ty,
        } => {
            if opts.surface {
                // Leading `Term::Let`s in the trailing branch blocks
                // render as statements via `render_block_body_opts`.
                format!(
                    "if!({}) {{ {} }} else {{ {} }}",
                    render_term_opts_with_context(cond, opts, context),
                    render_block_body_opts_with_context(then, opts, context),
                    render_block_body_opts_with_context(else_, opts, context),
                )
            } else {
                debug_assert_eq!(then_ty, else_ty);
                let result_ty = render_type_opts(then_ty, opts);
                format!(
                    "__if_then_else__({}, {}, .() {{ {} }}, .() {{ {} }})",
                    result_ty,
                    render_term_opts_with_context(cond, opts, context),
                    render_term_opts_with_context(then, opts, context),
                    render_term_opts_with_context(else_, opts, context),
                )
            }
        }
    }
}

fn literal_annotation(literal: &Term) -> &'static str {
    match literal {
        Term::NumLit { base, .. } => base.type_name(),
        Term::StrLit(_) => "String",
        Term::BoolLit(_) => "Bool",
        _ => unreachable!("LiteralAliasRef fallback must be a literal term"),
    }
}

/// If `t` is a `__pair__` call (with the canonical 2 type-args + 2
/// value-args shape), return the chain of value-args treating nested
/// right-associated `__pair__`s as further tuple elements:
/// `__pair__(T1, T2, a, b)` → `Some([a, b])`,
/// `__pair__(T1, T_rest, a, __pair__(T2, T3, b, c))` → `Some([a, b, c])`.
fn collect_tuple_items(t: &Term) -> Option<Vec<Term>> {
    match t {
        Term::PolyCall {
            name,
            type_args,
            args,
        } if name == "__pair__" && type_args.len() == 2 && args.len() == 2 => {
            let mut items = vec![args[0].clone()];
            if let Some(rest) = collect_tuple_items(&args[1]) {
                items.extend(rest);
            } else {
                items.push(args[1].clone());
            }
            Some(items)
        }
        _ => None,
    }
}

/// Walk a term looking for any construct whose surface rendering
/// differs from its Kio' rendering — i.e., the rendered output
/// changes shape when `RenderOpts.surface` flips on. Returns true
/// iff at least one such construct is reachable from `t`. The
/// generator uses this to decide `prog.uses_surface`: enabling
/// surface mode for a program with no surface-renderable construct
/// would only misclassify the generated body-level Kio' signal.
///
/// Today's surface-affected constructs:
/// - `__pair__` calls (tuple-literal sugar, slice 2.4)
/// - `Term::IfElse` (ordinary `if!` block elaborator calls)
/// - `Term::LabelConstruct` (surface `{label = e}`, slice 2.7+2.8)
/// - `Term::Into` / `Term::Onto` / `Term::Iso` / `Term::Align` /
///   `Term::Ease` / `Term::Atom` (the six algebraic elaborators,
///   identity-coercion surface forms).
/// - `Term::ReorderSum` / `Term::ReorderProd` / `Term::NarrowSum` /
///   `Term::NarrowProd` / `Term::WidenSum` / `Term::WidenProd` /
///   `Term::FlattenSum` / `Term::FlattenProd` / `Term::OneSum` /
///   `Term::OneProd` / `Term::Fit` (the spine palette, also
///   identity-coercion surface forms).
/// - `Term::UserElaborator` (generated user-defined elaborator calls).
///
/// `Type::Label` in the program signature does not trigger surface
/// rendering today: the generator deliberately renders Label types as
/// their newtype name in every position (see `render_type_opts`'s
/// note for why); the surface-form coverage from 2.7+2.8 lives in
/// the value-construction sugar exclusively.
pub fn term_uses_surface_forms(t: &Term) -> bool {
    match t {
        Term::NumLit { .. } | Term::StrLit(_) | Term::BoolLit(_) | Term::UnitLit | Term::Var(_) => {
            false
        }
        Term::Lambda { body, .. } => term_uses_surface_forms(body),
        Term::App(f, args) => {
            term_uses_surface_forms(f) || args.iter().any(term_uses_surface_forms)
        }
        Term::Let { rhs, body, .. } => {
            term_uses_surface_forms(rhs) || term_uses_surface_forms(body)
        }
        Term::PolyCall { name, args, .. } => {
            name == "__pair__" || args.iter().any(term_uses_surface_forms)
        }
        Term::TypeMember { args, .. } => args.iter().any(term_uses_surface_forms),
        Term::IfElse { .. }
        | Term::LabelConstruct { .. }
        | Term::Into { .. }
        | Term::Onto { .. }
        | Term::Iso { .. }
        | Term::Align { .. }
        | Term::Ease { .. }
        | Term::Atom { .. }
        | Term::ReorderSum { .. }
        | Term::ReorderProd { .. }
        | Term::NarrowSum { .. }
        | Term::NarrowProd { .. }
        | Term::WidenSum { .. }
        | Term::WidenProd { .. }
        | Term::FlattenSum { .. }
        | Term::FlattenProd { .. }
        | Term::OneSum { .. }
        | Term::OneProd { .. }
        | Term::Fit { .. }
        | Term::Ufcs { .. }
        | Term::FnPlaceholder { .. }
        | Term::Placeholder { .. }
        | Term::LiteralAliasRef { .. }
        | Term::OpCall { .. }
        | Term::UserElaborator { .. }
        | Term::MatchBang { .. } => true,
    }
}

/// Render a Kio' string literal: double-quoted, with `"` and `\`
/// escaped. The generator only ever emits strings whose characters
/// don't need any other escapes.
fn render_str_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Render a program's `fn` declaration as Kio source. Surface
/// sugar is enabled iff `prog.uses_surface` is true; in that case
/// every surface-form recogniser the renderer knows about (tuple
/// sugar, `if!` blocks, label-value sugar, generated user
/// elaborators) fires.
pub fn render_fn_def(prog: &Program) -> String {
    let opts = RenderOpts {
        surface: prog.uses_surface,
    };
    let mut context = RenderContext::for_program(prog, &opts);
    let params: Vec<String> = prog
        .params
        .iter()
        .map(|(n, t)| {
            // Surface-mode as-pattern injection: when a public-fn
            // parameter's type is a `Product`, wrap the binder as
            // `name: (_gen_<n>_l: L, _gen_<n>_r: R)` — exercises
            // the as-pattern surface form on every generated
            // product-typed parameter. The body still references
            // `name`, so the destructured components are bound-but-
            // unused; the typer accepts this under the existing
            // dead-binding rule. Kio' mode reverts to plain
            // `name: T` because Kio' rejects the surface form.
            if opts.surface
                && let Type::Product(l, r) = t
            {
                format!(
                    "{n}: (_gen_{n}_l: {}, _gen_{n}_r: {})",
                    render_type_opts(l, &opts),
                    render_type_opts(r, &opts),
                )
            } else {
                format!("{n}: {}", render_type_opts(t, &opts))
            }
        })
        .collect();
    format!(
        "fn {}({}) -> {} {{\n  {}\n}}\n",
        prog.fn_def_name,
        params.join(", "),
        render_type_opts(&prog.ret, &opts),
        render_block_body_opts_with_context(&prog.body, &opts, &mut context)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> Term {
        Term::App(Box::new(Term::Var(name.to_string())), Vec::new())
    }

    fn match_bang(scrutinee: Term, left_body: Term, right_body: Term) -> Term {
        Term::MatchBang {
            scrutinee: Box::new(scrutinee),
            left_param: "left".to_string(),
            left_ty: Type::i32(),
            left_body: Box::new(left_body),
            right_param: "right".to_string(),
            right_ty: Type::str(),
            right_body: Box::new(right_body),
            ret_ty: Type::i32(),
        }
    }

    /// `render_base` returns the same string the spec uses for each
    /// base in type position.
    #[test]
    fn render_base_matches_type_name() {
        for b in Base::all() {
            assert_eq!(render_base(*b), b.type_name());
        }
    }

    /// `render_type` for a function type emits a product domain for
    /// n-ary functions.
    #[test]
    fn render_type_function_canonical_shape() {
        let t = Type::Fun(
            vec![Type::Base(Base::I32), Type::Base(Base::Bool)],
            Box::new(Type::Base(Base::Str)),
        );
        assert_eq!(render_type(&t), "(I32 & Bool) -> String");
    }

    /// Function domains still use aliases when the domain product
    /// matches one.
    #[test]
    fn render_type_function_domain_uses_product_alias() {
        let t = Type::Fun(
            vec![Type::i32(), Type::i32()],
            Box::new(Type::Base(Base::I128)),
        );
        assert_eq!(render_type(&t), "Intpair -> I128");
    }

    /// Alias substitution can also appear inside the domain spine.
    #[test]
    fn render_type_function_domain_uses_alias_tail() {
        let t = Type::Fun(
            vec![Type::i32(), Type::i32(), Type::i32()],
            Box::new(Type::Base(Base::I128)),
        );
        assert_eq!(render_type(&t), "(I32 & Intpair) -> I128");
    }

    /// Anonymous product / sum types are always parenthesized in
    /// Kio' source — the renderer must emit the parens.
    #[test]
    fn render_type_product_and_sum_are_parenthesized() {
        let prod = Type::Product(
            Box::new(Type::Base(Base::I32)),
            Box::new(Type::Base(Base::Bool)),
        );
        assert_eq!(render_type(&prod), "(I32 & Bool)");

        let sum = Type::Sum(
            Box::new(Type::Base(Base::I32)),
            Box::new(Type::Base(Base::Str)),
        );
        assert_eq!(render_type(&sum), "(I32 | String)");
    }

    /// The two well-known type aliases (`Intpair`, `Optint`) emit
    /// their alias name rather than the structural form, so the
    /// generator exercises the type-unfolding path in the typer.
    #[test]
    fn render_type_substitutes_known_aliases() {
        let intpair = Type::Product(Box::new(Type::i32()), Box::new(Type::i32()));
        assert_eq!(render_type(&intpair), "Intpair");
        let optint = Type::Sum(Box::new(Type::Unit), Box::new(Type::i32()));
        assert_eq!(render_type(&optint), "Optint");
    }

    #[test]
    fn surface_placeholder_lambda_uses_fixed_arg_stem() {
        let term = Term::FnPlaceholder {
            params: vec![
                ("p1".to_string(), Type::i32()),
                ("p2".to_string(), Type::i32()),
            ],
            ret: Type::i32(),
            body: Box::new(Term::Placeholder {
                index: 2,
                fallback_name: "p2".to_string(),
                ty: Type::i32(),
            }),
        };

        assert_eq!(
            render_term_opts(&term, &RenderOpts { surface: true }),
            ".arg. { arg2 }"
        );
        assert_eq!(
            render_term_opts(&term, &RenderOpts { surface: false }),
            ".(p1, p2) { p2 }"
        );
    }

    #[test]
    fn surface_match_bang_non_var_source_uses_collision_free_binding() {
        let body = Term::MatchBang {
            scrutinee: Box::new(Term::App(
                Box::new(Term::Var("_gen_match_source2".to_string())),
                vec![Term::Var("_gen_match_source1".to_string())],
            )),
            left_param: "_gen_match_source3".to_string(),
            left_ty: Type::i32(),
            left_body: Box::new(Term::Var("_gen_match_source4".to_string())),
            right_param: "right".to_string(),
            right_ty: Type::str(),
            right_body: Box::new(Term::Var("_gen_match_source4".to_string())),
            ret_ty: Type::i32(),
        };
        let program = Program {
            fn_def_name: "_gen_match_source0".to_string(),
            params: vec![("_gen_match_source1".to_string(), Type::i32())],
            ret: Type::i32(),
            body,
            user_elaborators: Vec::new(),
            uses_surface: true,
            surface_mode: true,
        };

        let rendered = render_fn_def(&program);
        assert!(rendered.contains(
            "scope! { let _gen_match_source5 = _gen_match_source2(_gen_match_source1); \
             match!(_gen_match_source5) {"
        ));
        assert_eq!(rendered.matches("_gen_match_source4").count(), 2);
    }

    #[test]
    fn surface_nested_match_sources_receive_distinct_bindings() {
        let inner_left = match_bang(
            call("left_source"),
            Term::NumLit {
                base: Base::I32,
                repr: "1".to_string(),
            },
            Term::NumLit {
                base: Base::I32,
                repr: "2".to_string(),
            },
        );
        let inner_right = match_bang(
            call("right_source"),
            Term::NumLit {
                base: Base::I32,
                repr: "3".to_string(),
            },
            Term::NumLit {
                base: Base::I32,
                repr: "4".to_string(),
            },
        );
        let term = match_bang(call("outer_source"), inner_left, inner_right);

        let rendered = render_term_opts(&term, &RenderOpts { surface: true });
        for (suffix, source) in [(0, "outer_source"), (1, "left_source"), (2, "right_source")] {
            assert_eq!(
                rendered
                    .matches(&format!("let _gen_match_source{suffix} = {source}()"))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn surface_nested_conditional_prefix_is_explicitly_grouped() {
        let inner = Term::IfElse {
            cond: Box::new(Term::BoolLit(true)),
            then: Box::new(Term::BoolLit(false)),
            else_: Box::new(Term::BoolLit(true)),
            then_ty: Type::bool(),
            else_ty: Type::bool(),
        };
        let term = Term::IfElse {
            cond: Box::new(inner),
            then: Box::new(Term::BoolLit(true)),
            else_: Box::new(Term::BoolLit(false)),
            then_ty: Type::bool(),
            else_ty: Type::bool(),
        };
        assert_eq!(
            render_term_opts(&term, &RenderOpts { surface: true }),
            "if!(if!(.t(Bool)) { .f(Bool) } else { .t(Bool) }) { .t(Bool) } else { .f(Bool) }"
        );
    }

    #[test]
    fn surface_match_bang_var_source_uses_a_product_block() {
        let term = match_bang(
            Term::Var("source".to_string()),
            Term::Var("left".to_string()),
            Term::Var("right".to_string()),
        );

        assert_eq!(
            render_term_opts(&term, &RenderOpts { surface: true }),
            "match!(source) { (.(left: I32) { left }, .(right: String) { right }) }"
        );
    }

    #[test]
    fn prime_match_bang_non_var_source_stays_byte_identical() {
        let term = match_bang(
            call("make_source"),
            Term::Var("left".to_string()),
            Term::Var("right".to_string()),
        );

        assert_eq!(
            render_term_opts(&term, &RenderOpts { surface: false }),
            "__either__(I32, String, I32, make_source(), .(left) { left }, .(right) { right })"
        );
    }

    #[test]
    fn surface_match_bang_non_var_rhs_is_rendered_once() {
        let term = match_bang(
            call("source_once"),
            Term::Var("left".to_string()),
            Term::Var("right".to_string()),
        );

        let rendered = render_term_opts(&term, &RenderOpts { surface: true });
        assert_eq!(rendered.matches("source_once()").count(), 1);
        assert_eq!(rendered.matches("_gen_match_source0").count(), 2);
        assert!(!rendered.contains(".()"));
    }

    #[test]
    fn render_surface_op_call_parenthesizes_operands() {
        let term = Term::OpCall {
            op_tokens: "<+>".to_string(),
            callee: "kio_gen_op_pick".to_string(),
            lhs: Box::new(Term::NumLit {
                base: Base::I32,
                repr: "1".to_string(),
            }),
            rhs: Box::new(Term::Let {
                name: "x".to_string(),
                ty: Type::i32(),
                rhs: Box::new(Term::NumLit {
                    base: Base::I32,
                    repr: "2".to_string(),
                }),
                body: Box::new(Term::Var("x".to_string())),
            }),
        };
        let opts = RenderOpts { surface: true };
        assert_eq!(
            render_term_opts(&term, &opts),
            "((1(I32)) <+> (.() { let x = 2(I32); x }()))"
        );
    }

    /// `Base` integer / float predicates are non-overlapping and
    /// cover every numeric base exactly once.
    #[test]
    fn base_integer_and_float_predicates_partition_numeric_bases() {
        for b in Base::all() {
            let int = b.is_integer();
            let flt = b.is_float();
            // No overlap.
            assert!(!(int && flt), "{b:?} reports both integer and float");
            // Str/Bool are neither.
            if matches!(b, Base::Str | Base::Bool) {
                assert!(!int && !flt, "{b:?} should be neither integer nor float");
            } else {
                assert!(int || flt, "{b:?} should be integer or float");
            }
        }
    }
}
