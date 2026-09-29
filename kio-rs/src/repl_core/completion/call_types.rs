use super::{ScopeCandidate, ScopeKind, Session};
use crate::ast::{Expr, Module, PathSegment};
use crate::scope_walk::{CallHead, public_call_head};
use crate::span::Span;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionCallHeads {
    source: Arc<str>,
    heads: BTreeMap<Vec<String>, CallHead>,
}

#[cfg(test)]
thread_local! {
    pub(super) static CALL_HEAD_WORK: std::cell::Cell<(usize, usize)> = const {
        std::cell::Cell::new((0, 0))
    };
}

impl SessionCallHeads {
    pub(super) fn new(
        session: &Session,
        surface: &Module,
        source: &str,
        bindings: &[ScopeCandidate],
        providers: &BTreeMap<String, Arc<str>>,
    ) -> Option<Self> {
        let analysis = session.analysis()?;
        let package = &analysis.root_package_lowered;
        let entry = package.module(session.current()?)?;
        if analysis.sources.get(&entry.file_path).map(String::as_str) != Some(source) {
            return None;
        }
        let module = &entry.module;
        let package_file = package.package_file();
        let env = crate::pass::typecheck_core::ModuleEnv::build(
            module,
            package_file.map(|entry| &entry.package_file),
            package_file.map(|entry| entry.package_name.as_str()),
            Some(package),
        )
        .ok()?;
        #[cfg(test)]
        CALL_HEAD_WORK.with(|work| {
            let (builds, lookups) = work.get();
            work.set((builds + 1, lookups));
        });

        let mut paths = Vec::new();
        let mut qualified = Vec::new();
        for binding in bindings {
            if matches!(binding.kind, ScopeKind::Function | ScopeKind::Variable) {
                paths.push(vec![PathSegment::new(
                    binding.label.clone(),
                    Span::new(0, 0),
                )]);
            } else if matches!(binding.kind, ScopeKind::Module | ScopeKind::Type) {
                qualified.push(vec![PathSegment::new(
                    binding.label.clone(),
                    Span::new(0, 0),
                )]);
            }
        }
        let mut visited = std::collections::BTreeSet::new();
        while let Some(head) = qualified.pop() {
            if !visited.insert(
                head.iter()
                    .map(|part| part.name.clone())
                    .collect::<Vec<_>>(),
            ) {
                continue;
            }
            let (selected, _) = crate::scope_walk::qualified_providers(
                surface,
                &head,
                crate::pass::parser::CursorSlot::Argument,
                |name| {
                    let parsed =
                        crate::pass::parser::parse_module_file_lazy(providers.get(name)?).ok()?;
                    (parsed.module.path.segments.join("/") == name).then_some(((), parsed.module))
                },
            );
            let selected: Vec<_> = selected.values().map(|(_, module)| module).collect();
            for candidate in crate::scope_walk::qualified_candidates(
                surface,
                &head,
                crate::pass::parser::CursorSlot::Argument,
                &selected,
            ) {
                let mut path = head.clone();
                path.push(PathSegment::new(candidate.label, Span::new(0, 0)));
                match candidate.kind {
                    crate::scope_walk::CandidateKind::Function => paths.push(path),
                    crate::scope_walk::CandidateKind::Type
                    | crate::scope_walk::CandidateKind::Module => qualified.push(path),
                    _ => {}
                }
            }
        }
        let mut elaborations = crate::pass::typecheck_full::Elaborations::default();
        let mut context = crate::pass::typecheck_core::TypeCtx::new(&env, &mut elaborations);
        let mut heads = BTreeMap::new();
        for path in &paths {
            let Ok(synth) =
                crate::pass::typecheck_core::synth_path(path, Span::new(0, 0), &mut context)
            else {
                continue;
            };
            heads.insert(
                path.iter().map(|part| part.name.clone()).collect(),
                public_call_head(synth.ty.as_type(), &env.alias_ctx()),
            );
        }
        Some(Self {
            source: Arc::from(source),
            heads,
        })
    }

    pub(super) fn get(&self, source: &str, callee: &Expr) -> CallHead {
        if self.source.as_ref() != source {
            return CallHead::Unknown;
        }
        let Expr::Path { segments, .. } = callee else {
            return CallHead::Unknown;
        };
        #[cfg(test)]
        CALL_HEAD_WORK.with(|work| {
            let (builds, lookups) = work.get();
            work.set((builds, lookups + 1));
        });
        let key: Vec<_> = segments.iter().map(|part| part.name.clone()).collect();
        self.heads.get(&key).copied().unwrap_or(CallHead::Unknown)
    }
}
