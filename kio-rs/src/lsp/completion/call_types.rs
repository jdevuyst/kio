use crate::ast::Expr;
use crate::cmd::check::LspAnalysis;
use crate::pass::typecheck_core::AliasCtx;
use crate::scope_walk::{CallHead, public_call_head};
use lsp_types::Uri;

#[cfg(test)]
thread_local! {
    pub(super) static INDEX_LOOKUPS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

pub(super) fn current_call_head(
    uri: &Uri,
    source: &str,
    callee: &Expr,
    analysis: Option<&LspAnalysis>,
) -> CallHead {
    let Some((analysis, module_path)) = analysis.and_then(|analysis| {
        let file = crate::lsp::util::uri_to_canonical(uri)?;
        if analysis.sources.get(&file).map(String::as_str) != Some(source) {
            return None;
        }
        Some((analysis, analysis.file_to_module.get(&file)?))
    }) else {
        return CallHead::Unknown;
    };
    let lookup = |span| {
        #[cfg(test)]
        INDEX_LOOKUPS.with(|count| count.set(count.get() + 1));
        analysis
            .position_index
            .type_at(module_path, span)
            .map(|ty| (span, ty))
    };
    let Some((span, ty)) = lookup(callee.span()).or_else(|| {
        let Expr::Path { segments, .. } = callee else {
            return None;
        };
        let span = segments.last()?.span;
        (span != callee.span()).then(|| lookup(span)).flatten()
    }) else {
        return CallHead::Unknown;
    };
    let package = &analysis.root_package_lowered;
    let empty = std::collections::HashMap::new();
    let binder_names = analysis.position_index.type_binders_at(module_path, span);
    let aliases = AliasCtx {
        local: &empty,
        cross_module: &empty,
        type_interner: None,
        source_module: package.module(module_path).map(|entry| &entry.module),
        package: Some(package),
        binder_locals: binder_names.as_ref(),
    };
    public_call_head(ty, &aliases)
}
