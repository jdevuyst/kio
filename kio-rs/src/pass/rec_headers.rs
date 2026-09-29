//! Phase-independent validation of `rec(loop)` declaration headers.

use crate::ast::{Kind, RecGroup, SignatureParam, Surface, Type, TypeParam};
use crate::error::Error;
use crate::span::Span;

#[cfg(feature = "surface")]
pub(crate) fn validate(group: &RecGroup<Surface>) -> Result<(), Error> {
    let diagnostics = collect_diagnostics(group);
    diagnostics
        .name
        .into_iter()
        .chain(diagnostics.ty)
        .chain(diagnostics.parse)
        .next()
        .map_or(Ok(()), Err)
}

/// Return every header diagnostic in compiler phase order. Contract
/// projection uses the complete set because other package checks can occupy
/// the intervening Use and Name tiers.
#[cfg(feature = "cli")]
pub(crate) fn validate_all(group: &RecGroup<Surface>) -> Vec<Error> {
    let diagnostics = collect_diagnostics(group);
    diagnostics
        .parse
        .into_iter()
        .chain(diagnostics.name)
        .chain(diagnostics.ty)
        .collect()
}

#[derive(Default)]
struct HeaderDiagnostics {
    parse: Vec<Error>,
    name: Vec<Error>,
    ty: Vec<Error>,
}

fn collect_diagnostics(group: &RecGroup<Surface>) -> HeaderDiagnostics {
    let mut diagnostics = HeaderDiagnostics::default();
    let mut seen: std::collections::HashMap<&str, Span> = std::collections::HashMap::new();
    for member in &group.members {
        if let Some(first_span) = seen.get(member.name.as_str()) {
            diagnostics.name.push(
                Error::name_res(
                    member.meta.span,
                    format!("duplicate recursive member `{}`", member.name),
                )
                .with_secondary(
                    *first_span,
                    format!("`{}` first declared here", member.name),
                ),
            );
            continue;
        }
        seen.insert(member.name.as_str(), member.meta.span);
    }
    let first = group.members.first().expect("non-empty rec group");
    let first_type_params = signature_type_params(&first.sig.params);
    let first_ret = &first.ret;
    for member in &group.members {
        for tp in signature_type_params(&member.sig.params) {
            if tp.effective_kind() != Kind::Star {
                let error = Error::type_(
                    tp.span,
                    format!(
                        "`rec(loop)` can only pack kind-* type parameters, but `{}` has kind {}",
                        tp.name,
                        tp.effective_kind()
                    ),
                )
                .with_help(
                    "recursive loop state uses existential packets, whose binders are kind-*",
                );
                diagnostics.ty.push(error);
            }
        }
    }
    for member in group.members.iter().skip(1) {
        let member_type_params = signature_type_params(&member.sig.params);
        if !type_params_shape_eq(&first_type_params, &member_type_params) {
            diagnostics.parse.push(
                Error::parse(
                    member.meta.span,
                    "`rec(loop)` mutual-recursion members must declare the same type \
                 parameters in the same order",
                )
                .with_help(
                    "mutual members share one public polymorphic interface; tail calls can \
                 still choose a fresh instantiation with `rec name(T, ...)`",
                ),
            );
        }
        if !type_shape_eq(first_ret, &member.ret) {
            diagnostics.parse.push(
                Error::parse(
                    member.ret.span(),
                    "`rec(loop)` mutual-recursion members must have the same return type",
                )
                .with_secondary(first_ret.span(), "the first member's return type is here"),
            );
        }
    }
    for member in &group.members {
        for value in member.sig.params.iter().filter_map(|param| match param {
            SignatureParam::Type(_) => None,
            SignatureParam::Value(value) => Some(value),
        }) {
            if value.ty.is_none() {
                diagnostics.parse.push(
                    Error::parse(
                        value.meta.span,
                        "`rec(loop)` value parameters must have type annotations",
                    )
                    .with_help(
                        "the recursion lowering builds an explicit loop-state type; annotate \
                     the parameter, e.g. `x: T`",
                    ),
                );
            }
        }
    }
    diagnostics
}

fn signature_type_params(params: &[SignatureParam<Surface>]) -> Vec<TypeParam> {
    params
        .iter()
        .filter_map(|param| match param {
            SignatureParam::Type(param) => Some(param.clone()),
            SignatureParam::Value(_) => None,
        })
        .collect()
}

fn type_params_shape_eq(a: &[TypeParam], b: &[TypeParam]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.name == b.name && a.kind == b.kind)
}

fn type_shape_eq(a: &Type<Surface>, b: &Type<Surface>) -> bool {
    match (a, b) {
        (
            Type::Path {
                segments: a_segments,
                args: a_args,
                ..
            },
            Type::Path {
                segments: b_segments,
                args: b_args,
                ..
            },
        ) => {
            a_segments
                .iter()
                .map(|segment| segment.as_str())
                .eq(b_segments.iter().map(|segment| segment.as_str()))
                && a_args.len() == b_args.len()
                && a_args.iter().zip(b_args).all(|(a, b)| type_shape_eq(a, b))
        }
        (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => true,
        (
            Type::Function {
                param: a_param,
                ret: a_ret,
                ..
            },
            Type::Function {
                param: b_param,
                ret: b_ret,
                ..
            },
        ) => type_shape_eq(a_param, b_param) && type_shape_eq(a_ret, b_ret),
        (
            Type::Product {
                left: a_left,
                right: a_right,
                ..
            }
            | Type::Sum {
                left: a_left,
                right: a_right,
                ..
            },
            Type::Product {
                left: b_left,
                right: b_right,
                ..
            }
            | Type::Sum {
                left: b_left,
                right: b_right,
                ..
            },
        ) if std::mem::discriminant(a) == std::mem::discriminant(b) => {
            type_shape_eq(a_left, b_left) && type_shape_eq(a_right, b_right)
        }
        (
            Type::Forall {
                param: a_param,
                body: a_body,
                ..
            },
            Type::Forall {
                param: b_param,
                body: b_body,
                ..
            },
        ) => {
            a_param.name == b_param.name
                && a_param.kind == b_param.kind
                && type_shape_eq(a_body, b_body)
        }
        (
            Type::LabelSugar {
                labels: a_labels, ..
            },
            Type::LabelSugar {
                labels: b_labels, ..
            },
        ) => {
            a_labels.len() == b_labels.len()
                && a_labels.iter().zip(b_labels).all(|(a, b)| {
                    a.label == b.label
                        && match (&a.payload, &b.payload) {
                            (Some(a), Some(b)) => type_shape_eq(a, b),
                            (None, None) => true,
                            _ => false,
                        }
                })
        }
        (Type::Infer { .. }, Type::Infer { .. }) => true,
        _ => false,
    }
}
