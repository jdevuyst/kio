//! Canonical, phase-independent normalization of a contract type into a
//! string that is **alpha+kind-equal** iff two types share it.
//!
//! The compat check (`super::verdict`) compares the live package's
//! contract surface (a typechecked `Prime` package) against the
//! replayed signature changelog (`Surface`-phase declarations). Both
//! sides reach this module as a [`Type<P>`]; the normalizer reads only
//! the structural variants every phase shares (`Path` / `Unit` /
//! `Bottom` / `Function` / `Product` / `Sum` / `Forall`) and renders a
//! canonical string. Variants that are surface-only and uninhabited in
//! Kio′ (`LabelSugar`, `Infer`) are not expected here — a sig file is
//! Kio′ and the live side is post-typecheck — so they normalize to a
//! distinct sentinel that never alpha-matches a real type rather than
//! panicking.
//!
//! v1 identity (per `specs/versioning.md` §§ Strictness /
//! Per-kind type identity) is **strict**: alpha-equivalence of bound
//! type variables, with binder *kinds* part of the identity, and
//! nominal heads compared by their fully module-qualified name. There
//! is no variance-aware loosening, so any non-alpha-equal retained
//! type is a difference here.

use crate::ast::{Phase, Type, TypeParam};
use std::collections::HashMap;

/// Render `ty` to its canonical alpha+kind-normalized string.
///
/// `qualify_head` maps a *head* path's segments (the nominal type's
/// written reference) to its fully module-qualified segment list — the
/// per-side reference resolution (the live side resolves through the
/// defining module's `import` clauses; the sig side through the section's
/// `import` clauses). Type-parameter references (bound by an enclosing
/// `Forall`) are renamed to canonical De-Bruijn-ish names and never
/// passed to `qualify_head`.
pub fn canonical<P: Phase>(
    ty: &Type<P>,
    qualify_head: &impl Fn(&[String]) -> Vec<String>,
) -> String {
    let mut ctx = Ctx {
        bound: HashMap::new(),
        depth: 0,
        qualify_head,
    };
    let mut out = String::new();
    ctx.render(ty, &mut out);
    out
}

/// Canonicalize a declaration body under its out-of-band universal and
/// existential binder lists.
pub fn canonical_declaration<P: Phase>(
    ty: &Type<P>,
    universals: &[TypeParam],
    existentials: &[TypeParam],
    qualify_head: &impl Fn(&[String]) -> Vec<String>,
) -> String {
    let mut ctx = Ctx {
        bound: HashMap::new(),
        depth: 0,
        qualify_head,
    };
    // Install declaration binders before asking the side-specific qualifier
    // about any head. A later same-spelled nominal declaration therefore
    // cannot capture an existing binder or change its contract identity.
    for param in universals.iter().chain(existentials) {
        let level = ctx.depth;
        ctx.depth += 1;
        ctx.bound.insert(param.name.clone(), level);
    }
    let mut out = String::new();
    for param in existentials {
        out.push_str("exists<");
        out.push_str(&param.effective_kind().to_string());
        out.push('>');
    }
    ctx.render(ty, &mut out);
    out
}

struct Ctx<'a, F: Fn(&[String]) -> Vec<String>> {
    /// Bound type-variable name → its canonical De-Bruijn level.
    bound: HashMap<String, usize>,
    depth: usize,
    qualify_head: &'a F,
}

impl<F: Fn(&[String]) -> Vec<String>> Ctx<'_, F> {
    fn render<P: Phase>(&mut self, ty: &Type<P>, out: &mut String) {
        match ty {
            Type::Path { segments, args, .. } => {
                let names: Vec<String> = segments.iter().map(|s| s.name.clone()).collect();
                // A bare, single-segment head that is a bound type
                // variable renders by its canonical level, so two
                // alpha-equivalent foralls share a string.
                if names.len() == 1
                    && let Some(level) = self.bound.get(&names[0])
                {
                    out.push_str("#tv");
                    out.push_str(&level.to_string());
                } else {
                    let qualified = (self.qualify_head)(&names);
                    out.push('@');
                    out.push_str(&qualified.join("/"));
                }
                if !args.is_empty() {
                    out.push('(');
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        self.render(arg, out);
                    }
                    out.push(')');
                }
            }
            Type::Unit { .. } => out.push('.'),
            Type::Bottom { .. } => out.push('!'),
            Type::Function { param, ret, .. } => {
                out.push_str("fn(");
                self.render(param, out);
                out.push_str(")->");
                self.render(ret, out);
            }
            Type::Product { left, right, .. } => {
                out.push_str("(&");
                self.render(left, out);
                out.push(',');
                self.render(right, out);
                out.push(')');
            }
            Type::Sum { left, right, .. } => {
                out.push_str("(|");
                self.render(left, out);
                out.push(',');
                self.render(right, out);
                out.push(')');
            }
            Type::Forall { param, body, .. } => {
                let level = self.depth;
                self.depth += 1;
                let shadowed = self.bound.insert(param.name.clone(), level);
                out.push_str("forall<");
                // Binder kind is part of identity.
                out.push_str(&param.effective_kind().to_string());
                out.push('>');
                self.render(body, out);
                self.depth -= 1;
                match shadowed {
                    Some(prev) => {
                        self.bound.insert(param.name.clone(), prev);
                    }
                    None => {
                        self.bound.remove(&param.name);
                    }
                }
            }
            // Surface-only / inference holes do not occur in Kio′ nor in
            // a typechecked contract surface. Render a sentinel that can
            // never alpha-match a real type.
            Type::LabelSugar { .. } => out.push_str("#label-sugar#"),
            Type::Infer { .. } => out.push_str("#infer#"),
            Type::Goal { .. } => {
                unreachable!("open type goal cannot enter contract normalization")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Kind, Surface, TypeParam};
    use crate::span::Span;

    fn id(s: &[String]) -> Vec<String> {
        s.to_vec()
    }

    fn path(name: &str) -> Type<Surface> {
        Type::synth_path(vec![name.to_owned()], vec![], Span::new(0, 0))
    }

    #[test]
    fn alpha_equivalent_foralls_share_canonical() {
        let span = Span::new(0, 0);
        let a = Type::Forall {
            param: TypeParam {
                name: "A".into(),
                span,
                kind: None,
            },
            body: Box::new(Type::synth_function(vec![path("A")], path("A"), span)),
            meta: crate::ast::Meta::new(span),
        };
        let b = Type::Forall {
            param: TypeParam {
                name: "B".into(),
                span,
                kind: None,
            },
            body: Box::new(Type::synth_function(vec![path("B")], path("B"), span)),
            meta: crate::ast::Meta::new(span),
        };
        assert_eq!(canonical(&a, &id), canonical(&b, &id));
    }

    #[test]
    fn binder_kind_distinguishes() {
        let span = Span::new(0, 0);
        let star = Type::Forall {
            param: TypeParam {
                name: "F".into(),
                span,
                kind: None,
            },
            body: Box::new(path("F")),
            meta: crate::ast::Meta::new(span),
        };
        let higher = Type::Forall {
            param: TypeParam {
                name: "F".into(),
                span,
                kind: Some(Kind::arrow_chain(1)),
            },
            body: Box::new(path("F")),
            meta: crate::ast::Meta::new(span),
        };
        assert_ne!(canonical(&star, &id), canonical(&higher, &id));
    }

    #[test]
    fn nominal_head_qualified() {
        let span = Span::new(0, 0);
        let ty: Type<Surface> = Type::synth_path(vec!["Foo".to_owned()], vec![], span);
        let qualify = |s: &[String]| {
            let mut v = vec!["a".to_owned(), "b".to_owned()];
            v.extend(s.iter().cloned());
            v
        };
        assert_eq!(canonical(&ty, &qualify), "@a/b/Foo");
    }

    #[test]
    fn distinct_shapes_differ() {
        let span = Span::new(0, 0);
        let prod = Type::Product {
            left: Box::new(path("A")),
            right: Box::new(path("B")),
            meta: crate::ast::Meta::new(span),
        };
        let sum = Type::Sum {
            left: Box::new(path("A")),
            right: Box::new(path("B")),
            meta: crate::ast::Meta::new(span),
        };
        assert_ne!(canonical(&prod, &id), canonical(&sum, &id));
    }

    #[test]
    fn declaration_binders_are_alpha_equivalent_and_existentials_are_explicit() {
        let span = Span::new(0, 0);
        let param = |name: &str| TypeParam {
            name: name.to_owned(),
            span,
            kind: None,
        };
        let pair = |a: &str, b: &str| Type::Product {
            left: Box::new(path(a)),
            right: Box::new(path(b)),
            meta: crate::ast::Meta::new(span),
        };
        let a = canonical_declaration(&pair("A", "Hidden"), &[param("A")], &[param("Hidden")], &id);
        let b = canonical_declaration(
            &pair("Value", "Sealed"),
            &[param("Value")],
            &[param("Sealed")],
            &id,
        );
        assert_eq!(a, b);
        assert!(
            a.starts_with("exists<*>"),
            "existential prefix is explicit: {a}"
        );
        assert_ne!(
            a,
            canonical_declaration(
                &pair("A", "Hidden"),
                &[param("A"), param("Hidden")],
                &[],
                &id
            )
        );
    }
}
