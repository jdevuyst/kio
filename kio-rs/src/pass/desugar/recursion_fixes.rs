use crate::ast::{RecCallAnnotation, RecCallMode, RecCallSyntax};
use crate::error::{Error, Fix, FixEdit};
use crate::span::Span;

pub(super) fn missing_marker(mut error: Error, start: u32) -> Error {
    // Tail position through elaborators and type-changing recursion still need
    // ordinary checking. The editor publishes only candidates that recheck.
    for marker in ["rec", "rec(poly)", "rec(cont)", "rec(poly, cont)"] {
        error = error.with_fix(Fix::maybe_incorrect(
            format!("Mark recursive call with `{marker}`"),
            vec![FixEdit::new(Span::new(start, start), format!("{marker} "))],
        ));
    }
    error.with_help("mark the call with `rec`; add `poly` when type arguments change and `cont` when the call is not in tail position")
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
    let desired: Vec<_> = [RecCallMode::Poly, RecCallMode::Cont]
        .into_iter()
        .filter(|mode| {
            if *mode == required_mode {
                required
            } else {
                modes.contains(mode)
            }
        })
        .collect();
    let mut error = error.with_fix(Fix::machine_applicable(
        format!(
            "{} `{}` {} this recursive call",
            if required { "Add" } else { "Remove" },
            required_mode.as_str(),
            if required { "to" } else { "from" }
        ),
        annotation_edits(site, modes, &desired),
    ));
    // Cont checking precedes poly checking. Reanalysis can select an atomic
    // repair when changing cont alone exposes a second annotation error.
    if required_mode == RecCallMode::Cont {
        let alternate: Vec<_> = [RecCallMode::Poly, RecCallMode::Cont]
            .into_iter()
            .filter(|mode| {
                if *mode == RecCallMode::Poly {
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
    let mut edits = syntax
        .annotations
        .iter()
        .filter_map(|token| match token {
            RecCallAnnotation::Mode { mode, span } if !desired.contains(mode) => {
                Some(FixEdit::new(*span, ""))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        if let Some(edit) = edits.first_mut() {
            edit.replacement = missing;
        } else if desired.first().is_some_and(|mode| !modes.contains(mode)) {
            let end = syntax.annotations[0].span().end;
            edits.push(FixEdit::new(Span::new(end, end), format!("{missing}, ")));
        } else if let Some(RecCallAnnotation::Mode { span, .. }) = syntax
            .annotations
            .iter()
            .rev()
            .find(|token| matches!(token, RecCallAnnotation::Mode { .. }))
        {
            edits.push(FixEdit::new(
                Span::new(span.end, span.end),
                format!(", {missing}"),
            ));
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
        assert_eq!(fixes.len(), 4);
        for (fix, marker) in
            fixes
                .iter()
                .zip(["rec ", "rec(poly) ", "rec(cont) ", "rec(poly, cont) "])
        {
            assert_eq!(fix.applicability, Applicability::MaybeIncorrect);
            assert!(!fix.scaffold);
            assert!(fix.follow_on_reanalysis_scope.is_none());
            assert_eq!(fix.edits.len(), 1);
            assert_eq!(fix.edits[0].span, Span::new(start, start));
            assert_eq!(fix.edits[0].replacement, marker);
        }
    }
}
