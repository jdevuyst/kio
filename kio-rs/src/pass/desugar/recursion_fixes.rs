use crate::ast::{RecCallAnnotation, RecCallMode, RecCallSyntax};
use crate::error::{Error, Fix, FixEdit};
use crate::span::Span;

pub(super) fn missing_marker(mut error: Error, start: u32) -> Error {
    // Tail position through elaborators and type-changing recursion still need
    // ordinary checking. The editor publishes only candidates that recheck.
    for marker in [
        "recur",
        "recur(poly)",
        "recur(cont)",
        "recur(poly, cont)",
        "recur(escape)",
        "recur(poly, escape)",
    ] {
        error = error.with_fix(Fix::maybe_incorrect(
            format!("Mark recursive call with `{marker}`"),
            vec![FixEdit::new(Span::new(start, start), format!("{marker} "))],
        ));
    }
    error.with_help("mark the call with `recur`; add `poly` when type arguments change, `cont` outside tail position, and `escape` across a closure boundary")
}

#[derive(Clone, Copy)]
pub(super) struct CallSite<'a> {
    pub syntax: &'a RecCallSyntax,
    pub callee: Span,
}

pub(super) fn mode(
    error: Error,
    site: CallSite<'_>,
    modes: &[RecCallMode],
    required_mode: RecCallMode,
    required: bool,
) -> Error {
    let desired: Vec<_> = [RecCallMode::Poly, RecCallMode::Cont, RecCallMode::Escape]
        .into_iter()
        .filter(|mode| {
            if *mode == required_mode {
                required
            } else {
                modes.contains(mode)
            }
        })
        .collect();
    let desired = RecCallMode::canonical_modes(&desired);
    let mut error = error.with_fix(Fix::machine_applicable(
        format!(
            "{} `{}` {} this recursive call",
            if required { "Add" } else { "Remove" },
            required_mode.as_str(),
            if required { "to" } else { "from" }
        ),
        annotation_edits(site, modes, &desired),
    ));
    // Annotation checks can expose another obligation after this one is fixed.
    // Reanalysis selects an atomic repair from these bounded alternatives.
    if matches!(required_mode, RecCallMode::Cont | RecCallMode::Escape) {
        let alternate: Vec<_> = [RecCallMode::Poly, RecCallMode::Cont, RecCallMode::Escape]
            .into_iter()
            .filter(|mode| {
                if *mode == RecCallMode::Poly {
                    !desired.contains(mode)
                } else {
                    desired.contains(mode)
                }
            })
            .collect();
        let alternate = RecCallMode::canonical_modes(&alternate);
        error = error.with_fix(Fix::maybe_incorrect(
            "Correct recursive call annotations",
            annotation_edits(site, modes, &alternate),
        ));
    }
    if required_mode == RecCallMode::Escape && !required {
        for toggle_poly in [false, true] {
            let alternate: Vec<_> = [RecCallMode::Poly, RecCallMode::Cont]
                .into_iter()
                .filter(|mode| {
                    if *mode == RecCallMode::Cont || toggle_poly {
                        !desired.contains(mode)
                    } else {
                        desired.contains(mode)
                    }
                })
                .collect();
            error = error.with_fix(Fix::maybe_incorrect(
                "Correct recursive call annotations",
                annotation_edits(site, modes, &alternate),
            ));
        }
    }
    error
}

fn annotation_edits(
    site: CallSite<'_>,
    modes: &[RecCallMode],
    desired: &[RecCallMode],
) -> Vec<FixEdit> {
    let syntax = site.syntax;
    let missing = desired
        .iter()
        .filter(|mode| !modes.contains(mode))
        .map(|mode| mode.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if syntax.annotations.is_empty() {
        return vec![FixEdit::new(
            Span::new(syntax.keyword.end, syntax.keyword.end),
            format!("({missing})"),
        )];
    }
    if desired.is_empty() {
        let adjacent = syntax.keyword.end == syntax.annotations[0].span().start
            && syntax
                .annotations
                .windows(2)
                .all(|tokens| tokens[0].span().end == tokens[1].span().start)
            && syntax.annotations.last().unwrap().span().end == site.callee.start;
        return syntax
            .annotations
            .iter()
            .enumerate()
            .map(|(index, token)| {
                FixEdit::new(token.span(), if adjacent && index == 0 { " " } else { "" })
            })
            .collect();
    }
    let slots = syntax
        .annotations
        .iter()
        .filter(|token| matches!(token, RecCallAnnotation::Mode { .. }))
        .count();
    let retained = slots.min(desired.len());
    let mut edits = Vec::new();
    let mut mode_index = 0;
    let mut comma_index = 0;
    for (index, token) in syntax.annotations.iter().enumerate() {
        match token {
            RecCallAnnotation::Mode { mode, span } => {
                let mut replacement = desired
                    .get(mode_index)
                    .map_or(String::new(), |mode| mode.as_str().to_owned());
                if mode_index + 1 == slots {
                    for mode in desired.iter().skip(slots) {
                        replacement.push_str(", ");
                        replacement.push_str(mode.as_str());
                    }
                }
                if replacement != mode.as_str() {
                    edits.push(FixEdit::new(*span, replacement));
                }
                mode_index += 1;
            }
            RecCallAnnotation::Punctuation(span)
                if index > 0 && index + 1 < syntax.annotations.len() =>
            {
                if comma_index + 1 >= retained {
                    edits.push(FixEdit::new(*span, ""));
                }
                comma_index += 1;
            }
            RecCallAnnotation::Punctuation(_) => {}
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Applicability;

    #[test]
    fn missing_marker_candidates_are_local_and_require_reanalysis() {
        let source = "module main; rec(drive) fn again(x: .) -> . { again(x) }";
        let module = crate::pass::parser::parse(source).unwrap();
        let error = super::super::desugar_module(module).unwrap_err();
        let start = source.rfind("again(x)").unwrap() as u32;
        let fixes = error.diagnostic().fixes();
        assert_eq!(fixes.len(), 6);
        for (fix, marker) in fixes.iter().zip([
            "recur ",
            "recur(poly) ",
            "recur(cont) ",
            "recur(poly, cont) ",
            "recur(escape) ",
            "recur(poly, escape) ",
        ]) {
            assert_eq!(fix.applicability, Applicability::MaybeIncorrect);
            assert!(!fix.scaffold);
            assert!(fix.follow_on_reanalysis_scope.is_none());
            assert_eq!(fix.edits.len(), 1);
            assert_eq!(fix.edits[0].span, Span::new(start, start));
            assert_eq!(fix.edits[0].replacement, marker);
        }
    }
}
