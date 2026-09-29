//! Terminal-free REPL completion core.
//!
//! The command parser, session model, and completion candidate set the
//! `kio repl` terminal front end and the browser wasm wrapper both draw
//! on live here so the two surfaces offer the same candidates. The
//! reedline line-editor glue (the [`Completer`](reedline::Completer) /
//! [`Hinter`](reedline::Hinter) / [`Validator`](reedline::Validator)
//! trait objects, the popup menu, ghost-text prediction) stays in
//! [`crate::repl::completion`], a thin adapter over the single
//! [`complete`] entry point.
//!
//! ## What completes — context-aware routing
//!
//! [`complete`] draws on two candidate sources:
//!
//! 1. **Command / argument candidates**, from the [`NameSet`] built off
//!    the session. At the **start of a line** the set is every `:command`
//!    spelling. In a command's **argument position** it depends on which
//!    command's argument is being completed — [`complete`] folds the
//!    command word through
//!    [`canonical_name`](crate::repl_core::commands::canonical_name) and
//!    looks the
//!    [`CompletionShape`](crate::repl_core::commands::CompletionShape) up:
//!    module paths for `:load`, item names / FQNs (plus operators /
//!    builtins for the wider shapes) for the name commands.
//! 2. **In-scope identifier candidates**, at an expression position — a
//!    bare (non-`:`) prompt line, or a `:normalize` argument. These come
//!    through the [`ScopeProvider`] seam so the feature is written against
//!    an interface, not a concrete source. [`AstScopeProvider`] uses
//!    parser-owned expression context and the current module's exact
//!    import scope through [`crate::scope_walk`], so locals bound in the
//!    input line itself complete too. Other loaded modules remain available
//!    to command browsing but do not add ambient expression bindings.
//!
//! ## Fuzzy completion match
//!
//! Within whatever slice the router selects, candidates are matched and
//! ranked by [`fuzzy_match`] rather than a strict prefix test, so `fac`
//! finds `factorial`, `i32` finds every `*_i32` function, and `MAIN`
//! finds `demo/main` case-insensitively. The scorer tiers matches —
//! exact prefix, case-insensitive prefix, substring, subsequence — so an
//! exact-prefix hit always ranks above a looser one; [`rank`] sorts the
//! slice best-match-first. Each [`Candidate`] carries the matched char
//! positions in `match_indices`, so the popup menu underlines exactly
//! the characters the query landed on.

use std::{ops::Range, sync::Arc};

use crate::pass::parser::{ExpressionParseContext, expression_parse_context};
use crate::repl_core::commands::{
    CommandMode, CompletionShape, all_command_spellings, canonical_name, command_mode,
    command_summary, completion_shape,
};
use crate::repl_core::session::Session;

mod call_types;
#[cfg(test)]
mod context_tests;

/// A ranked completion candidate. `label` is the text an accept inserts
/// (the replaced span comes from the enclosing [`Completion`]); `kind`
/// classifies it for the popup menu / browser dropdown; `description`
/// is the de-emphasised menu detail column; `match_indices` are the
/// char positions in `label` the query matched, for the menu's
/// highlight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The text an accept inserts.
    pub label: String,
    /// What sort of thing this candidate names.
    pub kind: CandidateKind,
    /// The de-emphasised menu detail column, when the candidate carries
    /// metadata worth surfacing.
    pub description: Option<String>,
    /// The char positions in `label` the query matched — the menu's
    /// highlight. Empty for an empty query (nothing to highlight).
    pub match_indices: Vec<usize>,
}

/// What sort of thing a [`Candidate`] names — the seam the popup menu
/// and the browser dropdown key their icon / kind badge on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// A `:command` spelling.
    Command,
    /// A module path (`:load` / `:unload` / `:ls`).
    Module,
    /// A declared item, by its declaration kind.
    Item(ItemKind),
    /// An operator token binding a function.
    Operator,
    /// A compiler-provided builtin name.
    Builtin,
    /// The `-v` flag of `:ls` / `:scope`.
    Flag,
    /// An in-scope identifier at an expression position, by its scope
    /// kind.
    Identifier(ScopeKind),
    /// A named choice admitted by the current language grammar.
    Keyword,
}

impl CandidateKind {
    /// A short, stable machine tag for the kind — the browser dropdown's
    /// kind badge and the terminal's kind hint read it.
    pub fn tag(self) -> &'static str {
        match self {
            CandidateKind::Command => "command",
            CandidateKind::Module => "module",
            CandidateKind::Item(kind) => kind.tag(),
            CandidateKind::Operator => "op",
            CandidateKind::Builtin => "builtin",
            CandidateKind::Flag => "flag",
            CandidateKind::Identifier(kind) => kind.tag(),
            CandidateKind::Keyword => "keyword",
        }
    }
}

/// The result of a completion request: the candidates (best match
/// first) and the byte range in the input line an accepted candidate
/// replaces. Every candidate replaces the same span — the word / token
/// under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The byte range in the input line an accepted candidate replaces.
    pub replace: Range<usize>,
    /// The ranked candidates, best match first.
    pub candidates: Vec<Candidate>,
}

/// The kind of an in-scope identifier. Mirrors the shared scope-walk's
/// [`crate::scope_walk::CandidateKind`] exactly, so [`AstScopeProvider`]
/// maps walk candidates 1:1 onto [`ScopeCandidate`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// A top-level or inner `fn`.
    Function,
    /// A local `let`-binding or `fn` value parameter.
    Variable,
    /// A literal-alias constant.
    Constant,
    /// A type alias, newtype, imported type, or other nominal type name.
    Type,
    /// A type parameter in a signature or alias / newtype header.
    TypeParameter,
    /// A qualified-import alias (`import m/mod as alias`).
    Module,
}

impl ScopeKind {
    /// A short, stable machine tag for the scope kind.
    fn tag(self) -> &'static str {
        match self {
            ScopeKind::Function => "fn",
            ScopeKind::Variable => "let",
            ScopeKind::Constant => "const",
            ScopeKind::Type => "type",
            ScopeKind::TypeParameter => "type-param",
            ScopeKind::Module => "module",
        }
    }

    /// The de-emphasised menu detail column for an in-scope identifier.
    fn description(self) -> &'static str {
        match self {
            ScopeKind::Function => "in scope — fn",
            ScopeKind::Variable => "in scope — let",
            ScopeKind::Constant => "in scope — literal",
            ScopeKind::Type => "in scope — type",
            ScopeKind::TypeParameter => "in scope — type parameter",
            ScopeKind::Module => "in scope — module",
        }
    }
}

/// A plain in-scope identifier candidate — `{ label, kind }`, matching
/// the shared scope-walk's [`crate::scope_walk::Candidate`] shape so the
/// coarse provider here and the walk-backed [`AstScopeProvider`] agree
/// on what they hand back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeCandidate {
    /// The identifier to complete.
    pub label: String,
    /// What kind of binder it names.
    pub kind: ScopeKind,
}

/// The source of in-scope identifier candidates for an expression-shaped
/// completion position — the completion feature draws source (2), the
/// identifiers in scope, through this seam, never against a concrete
/// implementation. [`AstScopeProvider`] walks the current module and input.
pub trait ScopeProvider {
    /// The identifiers in scope for `expr` with the cursor at byte
    /// `cursor` within it. `expr` is the isolated expression text (a bare
    /// prompt line, or a `:normalize` argument), `cursor` its offset
    /// within that text.
    fn in_scope(&self, expr: &str, cursor: usize) -> Vec<ScopeCandidate>;

    /// Complete one expression, retaining its parser-owned replacement range.
    fn completion(&self, expr: &str, cursor: usize) -> Completion {
        scope_completion(self, expr, cursor)
    }
}

/// A catalog-backed fixture for testing the router independently of parsing.
#[cfg(test)]
pub struct CatalogFixture<'a> {
    names: &'a NameSet,
}

#[cfg(test)]
impl<'a> CatalogFixture<'a> {
    /// A provider over the top-level names in `names`.
    pub fn new(names: &'a NameSet) -> Self {
        Self { names }
    }
}

#[cfg(test)]
impl ScopeProvider for CatalogFixture<'_> {
    fn in_scope(&self, _expr: &str, _cursor: usize) -> Vec<ScopeCandidate> {
        let mut out: Vec<ScopeCandidate> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for it in &self.names.items {
            let Some(kind) = scope_kind_of_item(it.kind) else {
                continue;
            };
            if seen.insert(it.short_name.as_str()) {
                out.push(ScopeCandidate {
                    label: it.short_name.clone(),
                    kind,
                });
            }
        }
        for builtin in &self.names.builtins {
            if seen.insert(builtin.name.as_str()) {
                out.push(ScopeCandidate {
                    label: builtin.name.clone(),
                    kind: ScopeKind::Function,
                });
            }
        }
        out
    }
}

/// The parser-owned expression provider projects the exact binders and
/// named grammar choices visible at the cursor. Incomplete input retains
/// the grammar's consumed scope facts. The current module's imports determine its outer scope;
/// the session-wide command catalog does not contribute expression names.
pub struct AstScopeProvider<'a> {
    names: &'a NameSet,
}

/// Session scope uses the current module and the same lexical input walk.
pub type SessionScopeProvider<'a> = AstScopeProvider<'a>;

impl<'a> AstScopeProvider<'a> {
    /// A provider over `names`, walking against its
    /// [`NameSet::current_module_src`] when a module is loaded.
    pub fn new(names: &'a NameSet) -> Self {
        Self { names }
    }
}

impl ScopeProvider for AstScopeProvider<'_> {
    fn in_scope(&self, expr: &str, cursor: usize) -> Vec<ScopeCandidate> {
        self.context_scope(expr, cursor).1
    }

    fn completion(&self, expr: &str, cursor: usize) -> Completion {
        let (probe, candidates, block_label) = self.context_scope(expr, cursor);
        let Some(context) = probe
            .facts
            .cursor
            .as_ref()
            .filter(|_| probe.facts.suppression.is_none())
        else {
            return Completion {
                replace: cursor..cursor,
                candidates: Vec::new(),
            };
        };
        let prefix = &expr[context.atom.prefix.start as usize..context.atom.prefix.end as usize];
        let mut scored = Vec::new();
        if let Some(label) = block_label
            && let Some(matched) = language_match(prefix, &label)
        {
            scored.push(ScoredCandidate {
                score: matched.score,
                candidate: Candidate {
                    label,
                    kind: CandidateKind::Keyword,
                    description: Some("trailing block label".to_owned()),
                    match_indices: matched.indices,
                },
            });
        }
        for candidate in candidates {
            let Some(fm) = language_match(prefix, &candidate.label) else {
                continue;
            };
            scored.push(ScoredCandidate {
                score: fm.score,
                candidate: Candidate {
                    label: candidate.label,
                    kind: CandidateKind::Identifier(candidate.kind),
                    description: Some(candidate.kind.description().to_owned()),
                    match_indices: fm.indices,
                },
            });
        }
        for keyword in crate::scope_walk::tooling_keywords(&probe) {
            let Some(fm) = language_match(prefix, keyword) else {
                continue;
            };
            scored.push(ScoredCandidate {
                score: fm.score,
                candidate: Candidate {
                    label: keyword.to_owned(),
                    kind: CandidateKind::Keyword,
                    description: None,
                    match_indices: fm.indices,
                },
            });
        }
        let fallback_operators;
        let operators = if let Some(operators) = &self.names.expression_operators {
            operators.as_ref()
        } else {
            fallback_operators = self
                .names
                .current_module_src
                .as_deref()
                .and_then(|source| crate::pass::parser::parse_module_file_lazy(source).ok())
                .map(|module| crate::scope_walk::expression_operators(&module.module, u32::MAX))
                .unwrap_or_default();
            &fallback_operators
        };
        for (label, grammar) in crate::scope_walk::tooling_operators(&probe, operators) {
            let Some(fm) = fuzzy_match(prefix, &label) else {
                continue;
            };
            scored.push(ScoredCandidate {
                score: fm.score,
                candidate: Candidate {
                    label,
                    kind: CandidateKind::Operator,
                    description: Some(grammar),
                    match_indices: fm.indices,
                },
            });
        }
        Completion {
            replace: context.atom.replacement.start as usize..context.atom.replacement.end as usize,
            candidates: rank(scored),
        }
    }
}

impl AstScopeProvider<'_> {
    fn context_scope<'a>(
        &self,
        expr: &'a str,
        cursor: usize,
    ) -> (
        crate::pass::parser::ToolingProbe<'a>,
        Vec<ScopeCandidate>,
        Option<String>,
    ) {
        use crate::pass::parser::{CursorSlot, probe_tooling};
        let module = self
            .names
            .current_module_src
            .as_deref()
            .and_then(|source| crate::pass::parser::parse_module_file_lazy(source).ok());
        let fallback_context = self
            .names
            .expression_parse_context
            .is_none()
            .then(|| {
                module
                    .as_ref()
                    .and_then(|module| expression_parse_context(&module.module).ok())
            })
            .flatten();
        let mut probe = probe_tooling(
            expr,
            None,
            Some(cursor as u32),
            self.names
                .expression_parse_context
                .as_deref()
                .or(fallback_context.as_ref()),
        );
        let call_head = probe.facts.cursor.as_ref().and_then(|cursor| {
            if probe.facts.suppression.is_some() || cursor.slot != CursorSlot::Argument {
                return None;
            }
            let call = cursor.call.as_ref()?;
            if call.has_prior_argument
                || crate::scope_walk::expression_callee_shadowed(&probe, &call.callee)
            {
                return None;
            }
            Some(
                self.names
                    .expression_call_heads
                    .as_ref()?
                    .get(self.names.current_module_src.as_deref()?, &call.callee),
            )
        });
        if call_head == Some(crate::scope_walk::CallHead::Value)
            && let Some(cursor) = &mut probe.facts.cursor
        {
            cursor.slot = CursorSlot::Value;
        }
        let mut candidates: Vec<_> = crate::scope_walk::tooling_candidates(&probe)
            .into_iter()
            .map(scope_candidate_from_walk)
            .collect();
        if let Some(context) = &probe.facts.cursor
            && let Some(path) = &context.path
            && probe.facts.suppression.is_none()
            && let Some(module) = &module
            && crate::scope_walk::tooling_path_allowed(&probe, Some(&module.module))
        {
            let (providers, _) = crate::scope_walk::qualified_providers(
                &module.module,
                &path.prefix,
                context.slot,
                |name| {
                    let source = self.names.expression_provider_sources.get(name)?;
                    let parsed = crate::pass::parser::parse_module_file_lazy(source).ok()?;
                    (parsed.module.path.segments.join("/") == name).then_some(((), parsed.module))
                },
            );
            let modules: Vec<_> = providers.values().map(|(_, module)| module).collect();
            candidates.extend(
                crate::scope_walk::qualified_candidates(
                    &module.module,
                    &path.prefix,
                    context.slot,
                    &modules,
                )
                .into_iter()
                .map(|candidate| ScopeCandidate {
                    label: candidate.label,
                    kind: scope_kind_from_walk(candidate.kind),
                }),
            );
        }
        if let Some(context) = &probe.facts.cursor
            && probe.facts.suppression.is_none()
            && context.path.is_none()
            && matches!(
                context.slot,
                CursorSlot::Value | CursorSlot::Type | CursorSlot::Argument
            )
        {
            let fallback_bindings;
            let bindings = if let Some(bindings) = &self.names.expression_bindings {
                bindings.as_ref()
            } else {
                fallback_bindings = module
                    .as_ref()
                    .map(|module| {
                        let mut bindings =
                            crate::scope_walk::in_scope_candidates(&module.module, u32::MAX);
                        bindings
                            .extend(crate::scope_walk::type_candidates(&module.module, u32::MAX));
                        bindings
                            .into_iter()
                            .map(scope_candidate_from_walk)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                &fallback_bindings
            };
            candidates.extend(
                bindings
                    .iter()
                    .filter(|candidate| match context.slot {
                        CursorSlot::Value => {
                            !matches!(candidate.kind, ScopeKind::Type | ScopeKind::TypeParameter)
                        }
                        CursorSlot::Type => matches!(
                            candidate.kind,
                            ScopeKind::Type | ScopeKind::TypeParameter | ScopeKind::Module
                        ),
                        CursorSlot::Argument => true,
                        _ => unreachable!("only identifier contexts have outer bindings"),
                    })
                    .cloned(),
            );
        }
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|candidate| seen.insert(candidate.label.clone()));
        let block_label = (|| {
            if probe.facts.suppression.is_some()
                || probe.facts.cursor.as_ref()?.slot != CursorSlot::BlockLabel
            {
                return None;
            }
            let fact = probe.facts.block_cursor.as_ref()?;
            let crate::pass::parser::BlockCursorRegion::Label(index) = fact.region else {
                return None;
            };
            let module = &module.as_ref()?.module;
            let header = crate::scope_walk::selected_block_header(
                module,
                fact.head.as_str(),
                u32::MAX,
                |name| {
                    let source = self.names.expression_provider_sources.get(name)?;
                    let parsed = crate::pass::parser::parse_module_file_lazy(source).ok()?;
                    (parsed.module.path.segments.join("/") == name).then_some(parsed.module)
                },
            )?;
            if !header.matches_prefix(&fact.labels) {
                return None;
            }
            header.blocks.get(index)?.1.clone()
        })();
        (probe, candidates, block_label)
    }
}

fn language_match(query: &str, target: &str) -> Option<FuzzyMatch> {
    if !query.is_empty()
        && query != "_"
        && crate::naming::starts_like_type_name(query)
            != crate::naming::is_type_reference_name(target)
    {
        return None;
    }
    fuzzy_match(query, target)
}

/// Map a scope-walk candidate onto the seam's [`ScopeCandidate`] shape.
/// The kinds correspond 1:1.
fn scope_candidate_from_walk(candidate: crate::scope_walk::Candidate) -> ScopeCandidate {
    ScopeCandidate {
        label: candidate.label,
        kind: scope_kind_from_walk(candidate.kind),
    }
}

fn scope_kind_from_walk(kind: crate::scope_walk::CandidateKind) -> ScopeKind {
    use crate::scope_walk::CandidateKind as WalkKind;
    match kind {
        WalkKind::Function => ScopeKind::Function,
        WalkKind::Variable => ScopeKind::Variable,
        WalkKind::Constant => ScopeKind::Constant,
        WalkKind::Type => ScopeKind::Type,
        WalkKind::TypeParameter => ScopeKind::TypeParameter,
        WalkKind::Module => ScopeKind::Module,
    }
}

/// The [`ScopeKind`] a declared item of `kind` presents as at an
/// expression position.
#[cfg(test)]
fn scope_kind_of_item(kind: ItemKind) -> Option<ScopeKind> {
    match kind {
        ItemKind::Fn | ItemKind::Op | ItemKind::Equiv => Some(ScopeKind::Function),
        ItemKind::Newtype | ItemKind::TypeAlias | ItemKind::Labels => Some(ScopeKind::Type),
        ItemKind::Literal => Some(ScopeKind::Constant),
        ItemKind::LabelForward => None,
    }
}

/// What kind of declaration an [`ItemEntry`] names. Mirrors the surface
/// [`crate::ast::Item`] variants the REPL surfaces as completion
/// candidates; the popup menu renders the [`kind_label`]-derived label
/// in its description column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Fn,
    Newtype,
    TypeAlias,
    Literal,
    Labels,
    LabelForward,
    Op,
    Equiv,
}

impl ItemKind {
    /// A short, stable machine tag for the item kind.
    fn tag(self) -> &'static str {
        match self {
            ItemKind::Fn => "fn",
            ItemKind::Newtype => "newtype",
            ItemKind::TypeAlias => "type",
            ItemKind::Literal => "literal",
            ItemKind::Labels => "labels",
            ItemKind::LabelForward => "label",
            ItemKind::Op => "op",
            ItemKind::Equiv => "equiv",
        }
    }
}

pub(crate) fn item_allowed_for_command(kind: ItemKind, command: &str) -> bool {
    kind != ItemKind::LabelForward
        || command_mode(canonical_name(command)) == Some(CommandMode::Fqn)
}

/// The description-column label for an item of `kind` declared with the
/// given visibility. `pub fn` and `fn` differ on visibility (the detail
/// a reader scanning the menu most wants); the type-level and operator
/// kinds carry no visibility distinction in the label.
///
/// [`ItemKind::Op`] never reaches an [`ItemEntry`] — an `op` declaration
/// becomes an [`OperatorEntry`] instead — but the arm is spelled out so
/// the match stays exhaustive over [`ItemKind`].
fn kind_label(kind: ItemKind, vis_pub: bool) -> &'static str {
    match kind {
        ItemKind::Fn if vis_pub => "pub fn",
        ItemKind::Fn => "fn",
        ItemKind::Newtype => "newtype",
        ItemKind::TypeAlias => "type",
        ItemKind::Literal => "literal",
        ItemKind::Labels => "labels",
        ItemKind::LabelForward => "label",
        ItemKind::Op => "op",
        ItemKind::Equiv => "equiv",
    }
}

/// One module the package defines — a [`CompletionShape::ModulePath`]
/// candidate. Carries summary metadata (item / import counts, loaded
/// status) the popup will render alongside the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleEntry {
    /// Slash module path (`pkg/main`).
    pub path: String,
    /// Number of top-level items the module declares (0 when the module
    /// isn't loaded, so its AST isn't in hand).
    pub item_count: usize,
    /// Number of `import` clauses the module carries (0 when not loaded).
    pub import_count: usize,
    /// Whether the module is currently loaded in the session.
    pub is_loaded: bool,
}

/// One declared item — a [`CompletionShape::NameOrFqn`] candidate. Both
/// the bare `short_name` and the `fqn` are offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemEntry {
    /// The item's short (un-qualified) name.
    pub short_name: String,
    /// The `module.name` fully-qualified name.
    pub fqn: String,
    /// What kind of declaration this is.
    pub kind: ItemKind,
    /// Slash path of the module that declares the item.
    pub source_module: String,
    /// Whether the declaration is `pub`-exported.
    pub vis_pub: bool,
}

/// One operator binding — its token spelling, the function it binds, and
/// the declaring module. Offered under operator-aware shapes
/// (`:signature` / `:source` / `:doc`), but **not** under name-only
/// shapes (`:t` / `:pure` / `:which` / `:refs`, which key on a value
/// binding or the bound-function name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorEntry {
    /// The complete tagged grammar inserted by an operator query completion.
    pub grammar: String,
    /// The declaration signature, including its ordinary callable targets.
    pub signature: String,
    /// Slash path of the module that declares the operator.
    pub source_module: String,
}

/// One compiler-provided builtin imported by a loaded module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinEntry {
    pub name: String,
    pub module: crate::builtin_docs::BuiltinModule,
}

/// The completion name set: the structured catalogue of everything the
/// REPL can offer in a command-argument position, split by what each
/// command shape consumes. The terminal front end rebuilds it after each
/// state-changing command; the browser wrapper rebuilds it per
/// completion request.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NameSet {
    /// Every module the package defines, loaded or not — the
    /// [`CompletionShape::ModulePath`] candidate pool.
    pub modules: Vec<ModuleEntry>,
    /// Every loaded module's declared items — the Name/FQN candidate
    /// pool.
    pub items: Vec<ItemEntry>,
    /// Every loaded module's operator bindings.
    pub operators: Vec<OperatorEntry>,
    /// Compiler-provided names brought in by builtin module imports.
    pub builtins: Vec<BuiltinEntry>,
    /// The current module's source text, when the session has one — the
    /// module providing the prompt's outer lexical scope.
    /// `None` leaves only bindings introduced in the prompt itself.
    pub current_module_src: Option<String>,
    /// Exact consumer-declared operator grammar for parsing live expressions in the
    /// current module. This is an opaque parser artifact, not a reconstruction
    /// from the completion-only [`OperatorEntry`] list.
    pub(crate) expression_parse_context: Option<Arc<ExpressionParseContext>>,
    /// Current lexical module bindings with import kind and visibility resolved
    /// against the session's exact loaded providers.
    pub(crate) expression_bindings: Option<Arc<[ScopeCandidate]>>,
    /// Exact loaded source snapshots reachable through current-module imports.
    pub(crate) expression_provider_sources: std::collections::BTreeMap<String, Arc<str>>,
    pub(crate) expression_operators: Option<Arc<[crate::ast::OperatorGrammar]>>,
    /// Public call heads resolved once for the current, typed session snapshot.
    pub(crate) expression_call_heads: Option<Arc<call_types::SessionCallHeads>>,
}

impl NameSet {
    /// The completion name set for `session`: every package-defined
    /// module (loaded or not, with counts + loaded status) plus each
    /// loaded module's items, operator bindings, the builtin names its
    /// builtin-module imports bring in, and the current module's source
    /// text for the AST scope-walk.
    pub fn from_session(session: &Session) -> NameSet {
        let loaded_paths: std::collections::BTreeSet<&str> = session.loaded_paths().collect();

        let modules: Vec<ModuleEntry> = session
            .package_module_paths()
            .into_iter()
            .map(|path| {
                let is_loaded = loaded_paths.contains(path.as_str());
                let (item_count, import_count) = match session.module(&path) {
                    Some(m) => (m.module.items.len(), m.module.imports.len()),
                    None => (0, 0),
                };
                ModuleEntry {
                    path,
                    item_count,
                    import_count,
                    is_loaded,
                }
            })
            .collect();

        let mut items: Vec<ItemEntry> = Vec::new();
        let mut operators: Vec<OperatorEntry> = Vec::new();
        let mut imports_intrinsics = false;
        let mut imports_comptime = false;
        for m in session.modules_in_load_order() {
            for item in &m.module.items {
                collect_item(&m.path, item, &mut items, &mut operators);
            }
            imports_intrinsics |= m
                .module
                .imports
                .iter()
                .any(|u| matches!(u.kind, crate::ast::ImportKind::Intrinsics));
            imports_comptime |= m
                .module
                .imports
                .iter()
                .any(|u| matches!(u.kind, crate::ast::ImportKind::Comptime));
        }
        let mut builtins = Vec::new();
        if imports_intrinsics {
            builtins.extend(
                crate::builtin_docs::docs_for_module(
                    crate::builtin_docs::BuiltinModule::Intrinsics,
                )
                .into_iter()
                .map(|doc| BuiltinEntry {
                    name: doc.name.to_owned(),
                    module: doc.module,
                }),
            );
        }
        if imports_comptime {
            builtins.extend(
                crate::builtin_docs::docs_for_module(crate::builtin_docs::BuiltinModule::Comptime)
                    .into_iter()
                    .map(|doc| BuiltinEntry {
                        name: doc.name.to_owned(),
                        module: doc.module,
                    }),
            );
        }

        let current_module_src = session.current().and_then(|current| {
            let entry = session.module(current)?;
            session.source_of(&entry.file_path).map(str::to_owned)
        });
        let expression_parse_context = session.current().and_then(|current| {
            let module = &session.module(current)?.module;
            expression_parse_context(module).ok().map(Arc::new)
        });
        let expression_operators = session.current().and_then(|current| {
            let module = &session.module(current)?.module;
            Some(Arc::from(crate::scope_walk::expression_operators(
                module,
                u32::MAX,
            )))
        });
        let expression_bindings = session.current().and_then(|current| {
            let module = &session.module(current)?.module;
            let mut bindings = crate::scope_walk::in_scope_candidates(module, u32::MAX);
            bindings.extend(crate::scope_walk::type_candidates(module, u32::MAX));
            let bindings: Vec<_> = bindings
                .into_iter()
                .filter_map(|mut candidate| {
                    if let crate::scope_walk::CandidateOrigin::Import { provider, .. } =
                        &candidate.origin
                    {
                        let provider = &session.module(provider)?.module;
                        let mut entries = crate::doc_entry::documented_items_module(provider)
                            .into_iter()
                            .filter(|entry| entry.name() == candidate.label);
                        let entry = entries.next()?;
                        if entries.next().is_some()
                            || !crate::pass::resolve::is_visible(entry.visibility(), &module.path)
                        {
                            return None;
                        }
                        match entry {
                            crate::doc_entry::DocEntry::Elaborator(_) => candidate.label.push('!'),
                            crate::doc_entry::DocEntry::LiteralAlias(_) => {
                                candidate.kind = crate::scope_walk::CandidateKind::Constant
                            }
                            _ => {}
                        }
                    }
                    Some(scope_candidate_from_walk(candidate))
                })
                .collect();
            Some(Arc::from(bindings))
        });
        let mut expression_provider_sources = std::collections::BTreeMap::new();
        let mut pending = session
            .current()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut visited = std::collections::HashSet::new();
        while let Some(path) = pending.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            let Some(loaded) = session.module(&path) else {
                continue;
            };
            for import in &loaded.module.imports {
                match &import.kind {
                    crate::ast::ImportKind::Selective { from, .. } => {
                        pending.push(from.segments.join("/"))
                    }
                    crate::ast::ImportKind::Qualified { path, .. } => {
                        pending.push(path.segments.join("/"))
                    }
                    _ => {}
                }
            }
            if session.current() != Some(path.as_str())
                && let Some(source) = session.source_of(&loaded.file_path)
            {
                expression_provider_sources.insert(path, Arc::from(source));
            }
        }

        let expression_call_heads = session.current().and_then(|current| {
            call_types::SessionCallHeads::new(
                session,
                &session.module(current)?.module,
                current_module_src.as_deref()?,
                expression_bindings.as_deref()?,
                &expression_provider_sources,
            )
            .map(Arc::new)
        });
        NameSet {
            modules,
            items,
            operators,
            builtins,
            current_module_src,
            expression_parse_context,
            expression_bindings,
            expression_provider_sources,
            expression_operators,
            expression_call_heads,
        }
    }

    /// Module-path candidates fuzzy-matching `query`, sorted best match
    /// first. The description column shows the loaded module's item /
    /// import counts, or that an unloaded module is available to load.
    fn module_candidates(&self, query: &str) -> Vec<Candidate> {
        let mut scored: Vec<ScoredCandidate> = Vec::new();
        for m in &self.modules {
            let Some(fm) = fuzzy_match(query, &m.path) else {
                continue;
            };
            let desc = if m.is_loaded {
                format!(
                    "module ({} items, {} imports)",
                    m.item_count, m.import_count
                )
            } else {
                "module (available to load)".to_owned()
            };
            scored.push(ScoredCandidate {
                score: fm.score,
                candidate: Candidate {
                    label: m.path.clone(),
                    kind: CandidateKind::Module,
                    description: Some(desc),
                    match_indices: fm.indices,
                },
            });
        }
        rank(scored)
    }

    /// Name / FQN candidates fuzzy-matching `query`, sorted best match
    /// first — the `:t` / `:pure` / `:refs` pool. Operators are **not**
    /// included.
    fn name_candidates(&self, query: &str) -> Vec<Candidate> {
        rank(self.name_scored(query))
    }

    /// The scored (unsorted) name / FQN candidates — shared with the
    /// operator- and builtin-aware pools, which append their extra
    /// candidates before the single final sort so the kinds interleave
    /// by score rather than trailing as separate blocks.
    fn name_scored(&self, query: &str) -> Vec<ScoredCandidate> {
        let mut scored: Vec<ScoredCandidate> = Vec::new();
        for it in &self.items {
            let desc = format!("{} — {}", kind_label(it.kind, it.vis_pub), it.source_module);
            if let Some(fm) = fuzzy_match(query, &it.short_name) {
                scored.push(ScoredCandidate {
                    score: fm.score,
                    candidate: Candidate {
                        label: it.short_name.clone(),
                        kind: CandidateKind::Item(it.kind),
                        description: Some(desc.clone()),
                        match_indices: fm.indices,
                    },
                });
            }
            if let Some(fm) = fuzzy_match(query, &it.fqn) {
                scored.push(ScoredCandidate {
                    score: fm.score,
                    candidate: Candidate {
                        label: it.fqn.clone(),
                        kind: CandidateKind::Item(it.kind),
                        description: Some(desc),
                        match_indices: fm.indices,
                    },
                });
            }
        }
        scored
    }

    /// Name / FQN / operator candidates for `:signature` / `:source`,
    /// sorted best match first.
    fn name_fqn_op_candidates(&self, query: &str) -> Vec<Candidate> {
        let mut scored = self.name_scored(query);
        scored.extend(self.operator_scored(query));
        rank(scored)
    }

    /// Name / FQN / builtin candidates for `:which`.
    fn name_builtin_candidates(&self, query: &str) -> Vec<Candidate> {
        let mut scored = self.name_scored(query);
        scored.extend(self.builtin_scored(query));
        rank(scored)
    }

    /// Name / FQN / operator / builtin candidates for `:doc`.
    fn name_fqn_op_builtin_candidates(&self, query: &str) -> Vec<Candidate> {
        let mut scored = self.name_scored(query);
        scored.extend(self.operator_scored(query));
        scored.extend(self.builtin_scored(query));
        rank(scored)
    }

    fn operator_scored(&self, query: &str) -> Vec<ScoredCandidate> {
        let mut scored = Vec::new();
        for op in &self.operators {
            if let Some(fm) = fuzzy_match(query, &op.grammar) {
                let desc = op.signature.clone();
                scored.push(ScoredCandidate {
                    score: fm.score,
                    candidate: Candidate {
                        label: op.grammar.clone(),
                        kind: CandidateKind::Operator,
                        description: Some(desc),
                        match_indices: fm.indices,
                    },
                });
            }
        }
        scored
    }

    fn builtin_scored(&self, query: &str) -> Vec<ScoredCandidate> {
        let mut scored = Vec::new();
        for builtin in &self.builtins {
            let Some(fm) = fuzzy_match(query, &builtin.name) else {
                continue;
            };
            let desc = format!(
                "{} — {}",
                builtin_completion_label(builtin.module),
                builtin.module.name()
            );
            scored.push(ScoredCandidate {
                score: fm.score,
                candidate: Candidate {
                    label: builtin.name.clone(),
                    kind: CandidateKind::Builtin,
                    description: Some(desc),
                    match_indices: fm.indices,
                },
            });
        }
        scored
    }
}

fn builtin_completion_label(module: crate::builtin_docs::BuiltinModule) -> &'static str {
    match module {
        crate::builtin_docs::BuiltinModule::Intrinsics => "intrinsic",
        crate::builtin_docs::BuiltinModule::Comptime => "compile-time helper",
    }
}

/// Classify one Surface item into the structured completion entries it
/// contributes.
///
/// Fixed and variadic operators use their complete tagged grammar and shared
/// documentation signature. Other named declarations produce item entries.
fn collect_item(
    module_path: &str,
    item: &crate::ast::Item<crate::ast::Surface>,
    items: &mut Vec<ItemEntry>,
    operators: &mut Vec<OperatorEntry>,
) {
    use crate::ast::Item;

    if matches!(item, Item::Op(_, _) | Item::VariadicOperator(_, _)) {
        for entry in crate::doc_entry::documented_entries_for_item(item) {
            operators.push(OperatorEntry {
                grammar: entry.name().to_owned(),
                signature: entry.signature(),
                source_module: module_path.to_owned(),
            });
        }
        return;
    }

    let (name, kind, vis_pub) = match item {
        Item::FnDef(d) => (d.name.as_str(), ItemKind::Fn, d.vis.is_pub()),
        Item::RecGroup(g, _) => {
            for d in &g.members {
                items.push(ItemEntry {
                    short_name: d.name.clone(),
                    fqn: format!("{module_path}.{}", d.name),
                    kind: ItemKind::Fn,
                    source_module: module_path.to_owned(),
                    vis_pub: d.vis.is_pub(),
                });
            }
            return;
        }
        Item::TypeRecGroup(group) => {
            for member in &group.members {
                let (name, kind, vis_pub) = match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        (alias.name.as_str(), ItemKind::TypeAlias, alias.vis.is_pub())
                    }
                    crate::ast::TypeRecMember::Newtype(newtype) => (
                        newtype.name.as_str(),
                        ItemKind::Newtype,
                        newtype.vis.is_pub(),
                    ),
                    crate::ast::TypeRecMember::Labels(labels, _) => {
                        let Some(name) = labels.type_alias_name.as_deref() else {
                            continue;
                        };
                        (name, ItemKind::Labels, labels.vis.is_pub())
                    }
                };
                items.push(ItemEntry {
                    short_name: name.to_owned(),
                    fqn: format!("{module_path}.{name}"),
                    kind,
                    source_module: module_path.to_owned(),
                    vis_pub,
                });
            }
            return;
        }
        Item::Newtype(n) => (n.name.as_str(), ItemKind::Newtype, n.vis.is_pub()),
        Item::TypeAlias(a) => (a.name.as_str(), ItemKind::TypeAlias, a.vis.is_pub()),
        Item::LiteralAlias(l, _) => (l.name.as_str(), ItemKind::Literal, l.vis.is_pub()),
        Item::Equiv(e, _) => (e.name.as_str(), ItemKind::Equiv, false),
        Item::Elaborator(s, _) => (s.name.as_str(), ItemKind::Fn, s.vis.is_pub()),
        Item::HostType(h) => (h.name.as_str(), ItemKind::Newtype, true),
        Item::HostFn(h) => (h.name.as_str(), ItemKind::Fn, true),
        Item::Labels(t, _) => match &t.type_alias_name {
            Some(n) => (n.as_str(), ItemKind::Labels, t.vis.is_pub()),
            None => return,
        },
        Item::LabelForward(forward, _) => {
            let name = format!("{{{}}}", forward.name);
            items.push(ItemEntry {
                fqn: format!("{module_path}.{name}"),
                short_name: name,
                kind: ItemKind::LabelForward,
                source_module: module_path.to_owned(),
                vis_pub: forward.vis.is_pub(),
            });
            return;
        }
        Item::Op(_, _) => return,
        Item::VariadicOperator(_, _) => return,
    };
    items.push(ItemEntry {
        short_name: name.to_owned(),
        fqn: format!("{module_path}.{name}"),
        kind,
        source_module: module_path.to_owned(),
        vis_pub,
    });
}

/// A [`Candidate`] paired with its fuzzy-match score, the intermediate
/// the candidate builders collect before [`rank`] sorts them.
struct ScoredCandidate {
    score: i64,
    candidate: Candidate,
}

/// A fuzzy match of a query against a candidate: the ranking `score`
/// (higher is better) and the `indices` — the **char positions** in the
/// candidate the query matched, for the menu's highlighting.
struct FuzzyMatch {
    score: i64,
    indices: Vec<usize>,
}

/// The score-only form of [`fuzzy_match`] — the tier and ranking
/// assertions read more clearly against a bare score than against the
/// score-plus-indices struct.
#[cfg(test)]
fn fuzzy_score(query: &str, target: &str) -> Option<i64> {
    fuzzy_match(query, target).map(|m| m.score)
}

/// The base score of each match tier. The 20-point gaps leave room for a
/// within-tier bonus that can never push a candidate into the next tier.
const PREFIX_BASE: i64 = 100;
const CI_PREFIX_BASE: i64 = 80;
const SUBSTRING_BASE: i64 = 60;
const SUBSEQUENCE_BASE: i64 = 40;

/// The largest within-tier bonus. Bounded one below the 20-point gap
/// between adjacent tiers, so a bonus can never push a candidate up into
/// the next tier.
const MAX_TIER_BONUS: i64 = 19;

/// Score `target` against `query` and record the matched char positions,
/// returning [`None`] when `query` does not match at all. The tiers,
/// strongest first:
///
/// - **Exact prefix** (`fac` in `factorial`) — `PREFIX_BASE + len`.
/// - **Case-insensitive prefix** (`Fac` in `factorial`) —
///   `CI_PREFIX_BASE + len`.
/// - **Substring**, case-insensitive (`i32` in `add_i32`) —
///   `SUBSTRING_BASE + position-bonus`, earlier occurrences scoring
///   higher.
/// - **Subsequence**, case-insensitive (`ai3` in `add_i32`) —
///   `SUBSEQUENCE_BASE + adjacency-bonus`, tighter runs scoring higher.
///
/// `len` is the query's char count, so a longer exact prefix outranks a
/// shorter one. Every bonus is bounded by [`MAX_TIER_BONUS`] below the
/// gap to the next tier, so the tiers never cross. An empty query
/// matches everything at a flat baseline, with no matched indices.
fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: SUBSEQUENCE_BASE,
            indices: Vec::new(),
        });
    }

    let query_len = query.chars().count() as i64;
    let target_lower = target.to_lowercase();
    let query_lower = query.to_lowercase();

    if target.starts_with(query) {
        return Some(FuzzyMatch {
            score: PREFIX_BASE + query_len,
            indices: (0..query.chars().count()).collect(),
        });
    }
    if target_lower.starts_with(&query_lower) {
        return Some(FuzzyMatch {
            score: CI_PREFIX_BASE + query_len,
            indices: (0..query.chars().count()).collect(),
        });
    }

    if let Some(byte_pos) = target_lower.find(&query_lower) {
        let char_pos = target_lower[..byte_pos].chars().count();
        let position_bonus = (MAX_TIER_BONUS - char_pos as i64).max(1);
        let indices = (char_pos..char_pos + query.chars().count()).collect();
        return Some(FuzzyMatch {
            score: SUBSTRING_BASE + position_bonus,
            indices,
        });
    }

    subsequence_match(&query_lower, target)
}

/// Match `query_lower` (already lower-cased) as a subsequence of
/// `target`, case-insensitively: each query char must appear in `target`
/// in order. Returns the matched char positions and a score of
/// `SUBSEQUENCE_BASE` plus an adjacency bonus — the count of matched
/// chars that immediately follow the previous match. The bonus is
/// bounded below the subsequence → substring gap.
fn subsequence_match(query_lower: &str, target: &str) -> Option<FuzzyMatch> {
    let mut indices: Vec<usize> = Vec::new();
    let mut adjacency: i64 = 0;
    let mut query_chars = query_lower.chars().peekable();
    let mut next = query_chars.next();
    let mut last_matched: Option<usize> = None;

    for (char_pos, ch) in target.chars().enumerate() {
        let Some(q) = next else {
            break;
        };
        if ch.to_lowercase().eq(q.to_lowercase()) {
            if last_matched == Some(char_pos.wrapping_sub(1)) {
                adjacency += 1;
            }
            indices.push(char_pos);
            last_matched = Some(char_pos);
            next = query_chars.next();
        }
    }

    if next.is_none() {
        let bonus = adjacency.min(MAX_TIER_BONUS);
        Some(FuzzyMatch {
            score: SUBSEQUENCE_BASE + bonus,
            indices,
        })
    } else {
        None
    }
}

/// Sort scored candidates best match first (higher score earlier) and
/// drop the scores. The sort is **stable**, so equal-score candidates
/// keep their insertion order — items before their FQNs, declaration
/// order within a module — the tie-break a reader scanning the menu
/// expects.
fn rank(mut scored: Vec<ScoredCandidate>) -> Vec<Candidate> {
    scored.sort_by_key(|s| std::cmp::Reverse(s.score));
    scored.into_iter().map(|s| s.candidate).collect()
}

/// The byte offset at which the whitespace-delimited word under the
/// cursor starts: one past the last whitespace before `pos`, or 0 when
/// the cursor's word is the first on the line.
pub(crate) fn word_start(line: &str, pos: usize) -> usize {
    line[..pos]
        .rfind(char::is_whitespace)
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// The first whitespace-delimited token on `line`, with any leading `:`
/// stripped. Used to recover the command word for the shape lookup.
pub(crate) fn first_token(line: &str) -> &str {
    let trimmed = line.trim_start();
    let tok = trimmed
        .split_once(char::is_whitespace)
        .map(|(w, _)| w)
        .unwrap_or(trimmed);
    tok.strip_prefix(':').unwrap_or(tok)
}

/// The `:command` candidates fuzzy-matching `word`, sorted best match
/// first. Each candidate's description is the command's one-line `:help`
/// summary, looked up by the canonical name the spelling folds onto (so
/// a short synonym shows the same summary as its long form).
fn command_candidates_ranked(word: &str) -> Vec<Candidate> {
    let mut scored: Vec<ScoredCandidate> = Vec::new();
    for c in all_command_spellings() {
        let Some(fm) = fuzzy_match(word, &c) else {
            continue;
        };
        let canonical = canonical_name(c.strip_prefix(':').unwrap_or(&c));
        let desc = command_summary(canonical).map(str::to_owned);
        scored.push(ScoredCandidate {
            score: fm.score,
            candidate: Candidate {
                label: c,
                kind: CandidateKind::Command,
                description: desc,
                match_indices: fm.indices,
            },
        });
    }
    rank(scored)
}

/// The [`CompletionShape`] an argument-position word on `line` completes
/// to — the shared shape lookup the completer and the hinter both
/// consult so neither offers a candidate set the other wouldn't.
pub(crate) fn shape_for_position(line: &str) -> CompletionShape {
    completion_shape(canonical_name(first_token(line)))
}

/// Whether `canonical_command` accepts the `-v` (verbose-signature)
/// flag — `:ls` and `:scope`.
fn command_takes_verbose_flag(canonical_command: &str) -> bool {
    matches!(canonical_command, "ls" | "scope")
}

/// The `-v` flag candidate, offered when the user opens a flag word at a
/// `:ls` / `:scope` argument position.
fn verbose_flag_candidates(word: &str) -> Vec<Candidate> {
    if "-v".starts_with(word) {
        vec![Candidate {
            label: "-v".to_owned(),
            kind: CandidateKind::Flag,
            description: Some("show full signatures".to_owned()),
            match_indices: Vec::new(),
        }]
    } else {
        Vec::new()
    }
}

/// The candidates and replacement span for the cursor at byte `pos` in
/// `line`. Command / argument completion draws on `names`; the
/// no-candidate shapes and unrecognized commands yield nothing.
pub fn complete(names: &NameSet, scope: &dyn ScopeProvider, line: &str, pos: usize) -> Completion {
    // reedline's `only_buffer_difference` menu can call back with an
    // empty `line` paired with the full cursor offset on a no-change
    // repaint, so `pos > line.len()` is within contract. Clamp first.
    let pos = pos.min(line.len());
    let trimmed = line.trim_start();

    // An empty prompt offers the `:command` table for discovery; any
    // other bare (non-`:`) line is a bare-input query whose cursor
    // completes to the identifiers in scope (no Kio expression begins
    // with `:`).
    if trimmed.is_empty() {
        return Completion {
            replace: pos..pos,
            candidates: command_candidates_ranked(""),
        };
    }
    if !trimmed.starts_with(':') {
        return identifier_completion(scope, line, pos);
    }

    let word_start = word_start(line, pos);
    let word = &line[word_start..pos];
    let replace = word_start..pos;

    // At line start the candidate set is the `:command` table.
    if word_start == 0 {
        return Completion {
            replace,
            candidates: command_candidates_ranked(word),
        };
    }

    // The `-v` flag of `:ls` / `:scope`.
    if word.starts_with('-') && command_takes_verbose_flag(canonical_name(first_token(line))) {
        return Completion {
            replace,
            candidates: verbose_flag_candidates(word),
        };
    }

    let shape = shape_for_position(line);
    let mut candidates = match shape {
        // `:normalize <expr>` completes its argument to in-scope
        // identifiers, exactly as a bare expression line does.
        CompletionShape::Expression => return identifier_completion(scope, line, pos),
        CompletionShape::None => Vec::new(),
        CompletionShape::ModulePath => names.module_candidates(word),
        CompletionShape::NameOrFqn => names.name_candidates(word),
        CompletionShape::NameOrFqnOrBuiltin => names.name_builtin_candidates(word),
        CompletionShape::NameFqnOrOp => names.name_fqn_op_candidates(word),
        CompletionShape::NameFqnOpOrBuiltin => names.name_fqn_op_builtin_candidates(word),
    };
    candidates.retain(|candidate| match candidate.kind {
        CandidateKind::Item(kind) => item_allowed_for_command(kind, first_token(line)),
        _ => true,
    });
    Completion {
        replace,
        candidates,
    }
}

/// The in-scope identifier candidates for the cursor at byte `pos` in an
/// expression position (a bare prompt line, or a `:normalize` argument),
/// fuzzy-ranked against the identifier under the cursor. Draws the scope
/// through the [`ScopeProvider`] seam.
fn identifier_completion(scope: &dyn ScopeProvider, line: &str, pos: usize) -> Completion {
    let (expr_start, rel_cursor, _) = expression_target(line, pos);
    let expr = &line[expr_start..];
    let mut completion = scope.completion(expr, rel_cursor);
    completion.replace = completion.replace.start + expr_start..completion.replace.end + expr_start;
    completion
}

fn scope_completion(
    scope: &(impl ScopeProvider + ?Sized),
    expr: &str,
    cursor: usize,
) -> Completion {
    let start = ident_start(expr, cursor);
    let prefix = &expr[start..cursor];
    let mut scored: Vec<ScoredCandidate> = Vec::new();
    for candidate in scope.in_scope(expr, cursor) {
        if let Some(fm) = fuzzy_match(prefix, &candidate.label) {
            scored.push(ScoredCandidate {
                score: fm.score,
                candidate: Candidate {
                    label: candidate.label,
                    kind: CandidateKind::Identifier(candidate.kind),
                    description: Some(candidate.kind.description().to_owned()),
                    match_indices: fm.indices,
                },
            });
        }
    }
    Completion {
        replace: start..cursor,
        candidates: rank(scored),
    }
}

/// Resolve an expression-position cursor into `(expr_start, rel_cursor,
/// replace)`: the byte where the expression begins in `line`, the
/// cursor's offset within that expression, and the byte range in `line`
/// the identifier under the cursor occupies (what an accepted candidate
/// replaces). The ghost-text hinter reuses it to build its strict-prefix
/// pool.
pub(crate) fn expression_target(line: &str, pos: usize) -> (usize, usize, Range<usize>) {
    let expr_start = expression_start(line).min(pos);
    let expr = &line[expr_start..];
    let rel_cursor = pos - expr_start;
    let rel_ident_start = ident_start(expr, rel_cursor);
    let replace = (expr_start + rel_ident_start)..pos;
    (expr_start, rel_cursor, replace)
}

/// The start of the expression the REPL will parse: the whole bare input, or
/// an argument of a command that accepts expressions. Entity/path-only commands
/// do not gain expression parsing merely because their argument resembles Kio
/// syntax.
pub(crate) fn expression_input_start(line: &str) -> Option<usize> {
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    if !rest.starts_with(':') {
        return Some(0);
    }
    let command_end = rest.find(char::is_whitespace)?;
    let spelling = &rest[1..command_end];
    if !command_mode(canonical_name(spelling)).is_some_and(CommandMode::accepts_expression) {
        return None;
    }
    let (start, _, _) = expression_target(line, line.len());
    (start < line.len()).then_some(start)
}

/// The byte where the expression begins in `line`: 0 for a bare prompt
/// line (the whole line is the expression), or one past the command word
/// and the whitespace after it for a `:command <expr>` line.
fn expression_start(line: &str) -> usize {
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    if !rest.starts_with(':') {
        return 0;
    }
    match rest.find(char::is_whitespace) {
        None => line.len(),
        Some(rel_ws) => {
            let after_word = lead + rel_ws;
            match line[after_word..].find(|c: char| !c.is_whitespace()) {
                Some(off) => after_word + off,
                None => line.len(),
            }
        }
    }
}

/// The byte offset where the run of identifier characters ending at byte
/// `pos` in `s` begins — the identifier the cursor sits at the end of, or
/// `pos` itself when the char before the cursor is not an identifier
/// character.
fn ident_start(s: &str, pos: usize) -> usize {
    let mut start = pos;
    for (i, c) in s[..pos].char_indices().rev() {
        if is_ident_char(c) {
            start = i;
        } else {
            break;
        }
    }
    start
}

pub(crate) fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_name_matching_preserves_reserved_reference_roles() {
        for (query, target) in [
            ("__Ty", "__Type__"),
            ("___Ty", "___Type_name__"),
            ("_It", "_Item_type__"),
            ("item1_value2", "item1_value2"),
            ("__it", "__item_value"),
        ] {
            assert!(language_match(query, target).is_some(), "{query}: {target}");
        }
        for (query, target) in [
            ("__ty", "__Type__"),
            ("__Ty", "__type__"),
            ("_it", "_Item_type__"),
            ("_It", "_item_type__"),
        ] {
            assert!(language_match(query, target).is_none(), "{query}: {target}");
        }
    }

    /// Build a small `NameSet`: one loaded module `pkg/a` declaring a
    /// `fn foo` and a `fn bar`, an unloaded module `pkg/b`, and one
    /// operator `+` binding `add`.
    fn sample_set() -> NameSet {
        NameSet {
            modules: vec![
                ModuleEntry {
                    path: "pkg/a".to_owned(),
                    item_count: 2,
                    import_count: 1,
                    is_loaded: true,
                },
                ModuleEntry {
                    path: "pkg/b".to_owned(),
                    item_count: 0,
                    import_count: 0,
                    is_loaded: false,
                },
            ],
            items: vec![
                ItemEntry {
                    short_name: "foo".to_owned(),
                    fqn: "pkg/a.foo".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "pkg/a".to_owned(),
                    vis_pub: true,
                },
                ItemEntry {
                    short_name: "bar".to_owned(),
                    fqn: "pkg/a.bar".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "pkg/a".to_owned(),
                    vis_pub: false,
                },
            ],
            operators: vec![OperatorEntry {
                grammar: "op _ + __".to_owned(),
                signature: "op _ + __ { impl add }".to_owned(),
                source_module: "pkg/a".to_owned(),
            }],
            ..NameSet::default()
        }
    }

    /// `complete` over `names`, using the coarse [`CatalogFixture`]
    /// for the in-scope identifier source.
    fn run(names: &NameSet, line: &str, pos: usize) -> Completion {
        let provider = CatalogFixture::new(names);
        complete(names, &provider, line, pos)
    }

    /// The replacement values `complete` offers for `line` with the
    /// cursor at its end.
    fn values(names: &NameSet, line: &str) -> Vec<String> {
        run(names, line, line.len())
            .candidates
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    /// The description offered for the first candidate whose label
    /// equals `value` when completing `line`.
    fn description_for(names: &NameSet, line: &str, value: &str) -> Option<String> {
        run(names, line, line.len())
            .candidates
            .into_iter()
            .find(|c| c.label == value)
            .unwrap_or_else(|| panic!("no candidate with value {value:?} for {line:?}"))
            .description
    }

    #[test]
    fn first_token_strips_colon_and_leading_whitespace() {
        assert_eq!(first_token(":load pkg/a"), "load");
        assert_eq!(first_token("   :t foo"), "t");
        assert_eq!(first_token(":mods"), "mods");
        assert_eq!(first_token("foo bar"), "foo");
    }

    #[test]
    fn expression_input_start_uses_command_mode_not_argument_shape() {
        assert_eq!(expression_input_start("value"), Some(0));
        assert_eq!(expression_input_start("  value"), Some(0));
        assert_eq!(expression_input_start(":t .[A"), Some(3));
        assert_eq!(expression_input_start(":type .[A"), Some(6));
        assert_eq!(expression_input_start(":pure .[A"), Some(6));
        assert_eq!(expression_input_start(":normalize .[A"), Some(11));
        assert_eq!(expression_input_start(":norm .[A"), Some(6));
        assert_eq!(expression_input_start(":load .[A"), None);
        assert_eq!(expression_input_start(":source .[A"), None);
        assert_eq!(expression_input_start(":t"), None);
    }

    #[test]
    fn kind_label_covers_every_item_kind() {
        assert_eq!(kind_label(ItemKind::Fn, true), "pub fn");
        assert_eq!(kind_label(ItemKind::Fn, false), "fn");
        assert_eq!(kind_label(ItemKind::Newtype, true), "newtype");
        assert_eq!(kind_label(ItemKind::TypeAlias, false), "type");
        assert_eq!(kind_label(ItemKind::Literal, true), "literal");
        assert_eq!(kind_label(ItemKind::Labels, true), "labels");
        assert_eq!(kind_label(ItemKind::LabelForward, true), "label");
        assert_eq!(kind_label(ItemKind::Op, false), "op");
        assert_eq!(kind_label(ItemKind::Equiv, false), "equiv");
    }

    #[test]
    fn name_set_separates_modules_items_operators() {
        let set = sample_set();
        assert_eq!(set.modules.len(), 2);
        let a = set.modules.iter().find(|m| m.path == "pkg/a").unwrap();
        assert!(a.is_loaded);
        assert_eq!(a.item_count, 2);
        assert_eq!(a.import_count, 1);
        let b = set.modules.iter().find(|m| m.path == "pkg/b").unwrap();
        assert!(!b.is_loaded);
        assert_eq!(set.items.len(), 2);
        assert!(set.items.iter().any(|i| i.short_name == "foo" && i.vis_pub));
        assert!(
            set.items
                .iter()
                .any(|i| i.short_name == "bar" && !i.vis_pub)
        );
        assert_eq!(set.operators.len(), 1);
        assert_eq!(set.operators[0].grammar, "op _ + __");
        assert_eq!(set.operators[0].signature, "op _ + __ { impl add }");
    }

    #[test]
    fn complete_command_at_line_start() {
        let completion = run(&NameSet::default(), ":lo", 3);
        assert_eq!(completion.replace, 0..3);
        assert!(completion.candidates.iter().any(|c| c.label == ":load"));
    }

    #[test]
    fn complete_tolerates_pos_past_line_end() {
        let set = sample_set();
        let _ = run(&set, "", 3);
        let _ = run(&set, "ab", 5);
        let _ = run(&set, ":load p", 20);
    }

    #[test]
    fn complete_synonyms_are_offered() {
        let vals = values(&NameSet::default(), ":");
        assert!(vals.iter().any(|v| v == ":l"));
        assert!(vals.iter().any(|v| v == ":references"));
    }

    #[test]
    fn complete_load_argument_offers_only_modules() {
        let set = sample_set();
        assert_eq!(
            run(&set, ":load ", ":load ".len()).replace.start,
            ":load ".len()
        );
        let vals = values(&set, ":load ");
        assert!(vals.iter().any(|v| v == "pkg/a"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "pkg/b"), "got: {vals:?}");
        assert!(!vals.iter().any(|v| v == "foo"), "got: {vals:?}");
        assert!(!vals.iter().any(|v| v == "pkg/a.foo"), "got: {vals:?}");
    }

    #[test]
    fn complete_load_argument_prefix_filters() {
        let vals = values(&sample_set(), ":load pkg/a");
        assert_eq!(vals, vec!["pkg/a".to_owned()], "got: {vals:?}");
    }

    #[test]
    fn complete_t_argument_offers_names_and_fqns() {
        let set = sample_set();
        assert_eq!(run(&set, ":t ", ":t ".len()).replace.start, ":t ".len());
        let vals = values(&set, ":t ");
        assert!(vals.iter().any(|v| v == "foo"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "bar"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "pkg/a.foo"), "got: {vals:?}");
        assert!(!vals.iter().any(|v| v == "pkg/b"), "got: {vals:?}");
    }

    #[test]
    fn complete_pure_argument_offers_names_and_fqns() {
        let set = sample_set();
        let completion = run(&set, ":pure ", ":pure ".len());
        assert_eq!(completion.replace.start, ":pure ".len());
        let vals: Vec<String> = completion
            .candidates
            .into_iter()
            .map(|candidate| candidate.label)
            .collect();
        assert!(vals.iter().any(|value| value == "foo"), "got: {vals:?}");
        assert!(
            vals.iter().any(|value| value == "pkg/a.foo"),
            "got: {vals:?}"
        );
        assert!(
            !vals.iter().any(|value| value == "op _ + __"),
            "got: {vals:?}"
        );
        assert!(!vals.iter().any(|value| value == "pkg/b"), "got: {vals:?}");
    }

    #[test]
    fn complete_t_argument_synonym_folds() {
        assert!(values(&sample_set(), ":type ").iter().any(|v| v == "foo"));
    }

    #[test]
    fn complete_name_or_fqn_skips_operators() {
        assert!(values(&sample_set(), ":t +").is_empty());
    }

    #[test]
    fn complete_source_argument_offers_operators() {
        let vals = values(&sample_set(), ":source ");
        assert!(vals.iter().any(|v| v == "foo"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "pkg/a.foo"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "op _ + __"), "got: {vals:?}");
    }

    #[test]
    fn complete_doc_and_signature_offer_operators() {
        let set = sample_set();
        assert!(values(&set, ":doc ").iter().any(|v| v == "op _ + __"));
        assert!(values(&set, ":signature ").iter().any(|v| v == "op _ + __"));
    }

    #[test]
    fn complete_doc_and_which_offer_builtins_when_in_scope() {
        let set = NameSet {
            builtins: vec![
                BuiltinEntry {
                    name: "__left__".to_owned(),
                    module: crate::builtin_docs::BuiltinModule::Intrinsics,
                },
                BuiltinEntry {
                    name: "__reflect_type__".to_owned(),
                    module: crate::builtin_docs::BuiltinModule::Comptime,
                },
            ],
            ..NameSet::default()
        };
        assert!(
            values(&set, ":doc __ref")
                .iter()
                .any(|v| v == "__reflect_type__")
        );
        assert!(values(&set, ":which __le").iter().any(|v| v == "__left__"));
        assert!(
            values(&set, ":t __le").is_empty(),
            "`:t` should not offer builtin docs"
        );
        assert_eq!(
            description_for(&set, ":doc __ref", "__reflect_type__"),
            Some("compile-time helper — __comptime__".to_owned())
        );
    }

    #[test]
    fn module_candidate_carries_item_and_import_counts() {
        let set = NameSet {
            modules: vec![
                ModuleEntry {
                    path: "pkg/loaded".to_owned(),
                    item_count: 3,
                    import_count: 5,
                    is_loaded: true,
                },
                ModuleEntry {
                    path: "pkg/cold".to_owned(),
                    item_count: 0,
                    import_count: 0,
                    is_loaded: false,
                },
            ],
            ..NameSet::default()
        };
        assert_eq!(
            description_for(&set, ":load ", "pkg/loaded"),
            Some("module (3 items, 5 imports)".to_owned())
        );
        assert_eq!(
            description_for(&set, ":load ", "pkg/cold"),
            Some("module (available to load)".to_owned())
        );
    }

    #[test]
    fn name_candidate_carries_kind_and_source() {
        let set = NameSet {
            items: vec![
                ItemEntry {
                    short_name: "factorial".to_owned(),
                    fqn: "exec_factorial/main.factorial".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "exec_factorial/main".to_owned(),
                    vis_pub: true,
                },
                ItemEntry {
                    short_name: "helper".to_owned(),
                    fqn: "exec_factorial/main.helper".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "exec_factorial/main".to_owned(),
                    vis_pub: false,
                },
                ItemEntry {
                    short_name: "Token".to_owned(),
                    fqn: "demo/lex.Token".to_owned(),
                    kind: ItemKind::Newtype,
                    source_module: "demo/lex".to_owned(),
                    vis_pub: true,
                },
            ],
            ..NameSet::default()
        };
        assert_eq!(
            description_for(&set, ":t fac", "factorial"),
            Some("pub fn — exec_factorial/main".to_owned())
        );
        assert_eq!(
            description_for(&set, ":t help", "helper"),
            Some("fn — exec_factorial/main".to_owned())
        );
        assert_eq!(
            description_for(&set, ":t Tok", "Token"),
            Some("newtype — demo/lex".to_owned())
        );
        assert_eq!(
            description_for(&set, ":t demo/lex.Tok", "demo/lex.Token"),
            Some("newtype — demo/lex".to_owned())
        );
    }

    #[test]
    fn operator_candidate_carries_complete_signature() {
        let set = NameSet {
            operators: vec![OperatorEntry {
                grammar: "op _ + __".to_owned(),
                signature: "op _ + __ { impl add_i32 }".to_owned(),
                source_module: "demo/main".to_owned(),
            }],
            ..NameSet::default()
        };
        assert_eq!(
            description_for(&set, ":source ", "op _ + __"),
            Some("op _ + __ { impl add_i32 }".to_owned())
        );
    }

    #[test]
    fn command_candidate_carries_summary() {
        let set = NameSet::default();
        assert_eq!(
            description_for(&set, ":", ":load"),
            Some("load a module (and its import-closure)".to_owned())
        );
        assert_eq!(
            description_for(&set, ":", ":l"),
            Some("load a module (and its import-closure)".to_owned())
        );
    }

    #[test]
    fn complete_mods_argument_offers_nothing() {
        assert!(values(&sample_set(), ":mods ").is_empty());
    }

    #[test]
    fn complete_ls_scope_offer_the_verbose_flag() {
        let set = sample_set();
        assert!(values(&set, ":ls -").contains(&"-v".to_owned()));
        assert!(values(&set, ":list -").contains(&"-v".to_owned()));
        assert!(values(&set, ":scope -").contains(&"-v".to_owned()));
        assert!(!values(&set, ":load -").contains(&"-v".to_owned()));
        assert!(values(&set, ":ls -v pkg").contains(&"pkg/a".to_owned()));
    }

    #[test]
    fn complete_normalize_offers_in_scope_identifiers() {
        // `:normalize <expr>` completes its argument to in-scope
        // identifiers — the loaded module's top-level names — not module
        // paths or FQNs.
        let set = sample_set();
        let vals = values(&set, ":normalize fo");
        assert!(vals.iter().any(|v| v == "foo"), "got: {vals:?}");
        assert!(!vals.iter().any(|v| v == "pkg/a.foo"), "got: {vals:?}");
        assert!(!vals.iter().any(|v| v == "pkg/a"), "got: {vals:?}");
        // An empty name set offers nothing.
        assert!(values(&NameSet::default(), ":normalize fo").is_empty());
    }

    #[test]
    fn complete_normalize_replace_span_is_the_identifier() {
        // The replaced span covers only the identifier under the cursor,
        // not the whole argument — `:normalize foo(ba|` replaces `ba`.
        let set = fn_set(&["bar"]);
        let line = ":normalize foo(ba";
        let completion = run(&set, line, line.len());
        assert_eq!(completion.replace, (line.len() - 2)..line.len());
        assert!(completion.candidates.iter().any(|c| c.label == "bar"));
    }

    #[test]
    fn complete_bare_expression_offers_in_scope_identifiers() {
        // A bare (non-`:`) prompt line is a bare-input query, so it
        // completes to the identifiers in scope, keyed on the identifier
        // under the cursor rather than the whole line.
        let set = fn_set(&["factorial", "factory"]);
        let vals = values(&set, "1 + fac");
        assert!(vals.iter().any(|v| v == "factorial"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "factory"), "got: {vals:?}");
        // The replaced span covers just `fac`.
        let line = "1 + fac";
        assert_eq!(run(&set, line, line.len()).replace, 4..7);
    }

    #[test]
    fn complete_bare_identifier_carries_scope_kind() {
        let set = NameSet {
            items: vec![
                ItemEntry {
                    short_name: "run".to_owned(),
                    fqn: "demo/main.run".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "demo/main".to_owned(),
                    vis_pub: true,
                },
                ItemEntry {
                    short_name: "Rope".to_owned(),
                    fqn: "demo/main.Rope".to_owned(),
                    kind: ItemKind::Newtype,
                    source_module: "demo/main".to_owned(),
                    vis_pub: true,
                },
            ],
            ..NameSet::default()
        };
        let candidates = run(&set, "r", 1).candidates;
        let run_kind = candidates.iter().find(|c| c.label == "run").unwrap().kind;
        assert_eq!(run_kind, CandidateKind::Identifier(ScopeKind::Function));
        let rope_kind = candidates.iter().find(|c| c.label == "Rope").unwrap().kind;
        assert_eq!(rope_kind, CandidateKind::Identifier(ScopeKind::Type));
    }

    #[test]
    fn complete_empty_line_offers_command_discovery() {
        // An empty prompt still offers the `:command` table (the bare
        // expression path only fires once the user has typed something).
        let vals = values(&sample_set(), "");
        assert!(vals.iter().any(|v| v == ":load"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == ":normalize"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == ":pure"), "got: {vals:?}");
    }

    #[test]
    fn complete_unknown_command_offers_nothing() {
        assert!(values(&sample_set(), ":frobnicate ").is_empty());
    }

    #[test]
    fn session_scope_provider_dedups_and_maps_kinds() {
        // The coarse provider offers each distinct top-level name once,
        // plus builtins, mapping item kinds onto scope kinds — it ignores
        // the expr / cursor because module-level names are always in
        // scope.
        let set = NameSet {
            items: vec![
                ItemEntry {
                    short_name: "id".to_owned(),
                    fqn: "a.id".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "a".to_owned(),
                    vis_pub: true,
                },
                ItemEntry {
                    short_name: "id".to_owned(),
                    fqn: "b.id".to_owned(),
                    kind: ItemKind::Fn,
                    source_module: "b".to_owned(),
                    vis_pub: true,
                },
            ],
            builtins: vec![BuiltinEntry {
                name: "__left__".to_owned(),
                module: crate::builtin_docs::BuiltinModule::Intrinsics,
            }],
            ..NameSet::default()
        };
        let provider = CatalogFixture::new(&set);
        let scope = provider.in_scope("id", 2);
        assert_eq!(scope.iter().filter(|c| c.label == "id").count(), 1);
        assert!(scope.iter().any(|c| c.label == "__left__"));
    }

    /// Current-module source plus an independently populated command catalog.
    fn walk_set() -> NameSet {
        let src = "module demo/main;\n\
             pub fn factorial(n: .) -> . { n }\n\
             fn add(x: ., y: .) -> . { x }\n\
             op _ + __ { impl add }\n";
        NameSet {
            items: vec![ItemEntry {
                short_name: "factorial".to_owned(),
                fqn: "demo/main.factorial".to_owned(),
                kind: ItemKind::Fn,
                source_module: "demo/main".to_owned(),
                vis_pub: true,
            }],
            builtins: vec![BuiltinEntry {
                name: "__left__".to_owned(),
                module: crate::builtin_docs::BuiltinModule::Intrinsics,
            }],
            current_module_src: Some(src.to_owned()),
            ..NameSet::default()
        }
    }

    #[test]
    fn exact_scope_expression_excludes_ambient_session_names() {
        let mut set = walk_set();
        set.items.push(ItemEntry {
            short_name: "foreign_private".to_owned(),
            fqn: "other/module.foreign_private".to_owned(),
            kind: ItemKind::Fn,
            source_module: "other/module".to_owned(),
            vis_pub: false,
        });
        let provider = AstScopeProvider::new(&set);
        let scope = provider.in_scope("factorial", 9);
        assert!(scope.iter().any(|c| c.label == "factorial"));
        assert!(
            !scope
                .iter()
                .any(|c| c.label == "foreign_private" || c.label == "__left__")
        );
        let command = complete(&set, &provider, ":source foreign", 15);
        assert!(
            command
                .candidates
                .iter()
                .any(|c| c.label.contains("foreign_private"))
        );
    }

    #[test]
    fn ast_scope_provider_offers_input_locals_via_recovery() {
        // The prompt input binds `abc` in a lambda the user is still
        // typing — the walk over the recovered wrap offers it, with the
        // module's own fns alongside.
        let set = walk_set();
        let provider = AstScopeProvider::new(&set);
        let input = ".(abc: .) { ab";
        let scope = provider.in_scope(input, input.len());
        let abc = scope
            .iter()
            .find(|c| c.label == "abc")
            .expect("input-bound lambda param offered");
        assert_eq!(abc.kind, ScopeKind::Variable);
        let factorial = scope
            .iter()
            .find(|c| c.label == "factorial")
            .expect("module-level fn offered");
        assert_eq!(factorial.kind, ScopeKind::Function);
    }

    #[test]
    fn ast_scope_provider_uses_only_current_module_imports() {
        let set = walk_set();
        let provider = AstScopeProvider::new(&set);
        let scope = provider.in_scope("1 + fac", 7);
        assert_eq!(
            scope.iter().filter(|c| c.label == "factorial").count(),
            1,
            "one candidate per lexical binding"
        );
        assert!(!scope.iter().any(|c| c.label == "__left__"));
    }

    #[test]
    fn ast_scope_provider_without_module_has_no_ambient_names() {
        let set = NameSet {
            current_module_src: None,
            ..walk_set()
        };
        let fine = AstScopeProvider::new(&set).in_scope("1 + fac", 7);
        assert!(fine.is_empty());
    }

    #[test]
    fn block_call_completion_keeps_prompt_locals_without_a_recursive_owner() {
        let set = walk_set();
        let provider = AstScopeProvider::new(&set);
        for input in [
            "do! bind { let local <- (); ",
            "do! bind { (); let local = (); ",
            "do! bind { let local = (); ",
            "scope! { let local = (); ",
        ] {
            let result = complete(&set, &provider, input, input.len());
            assert!(
                result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.label == "local"),
                "{input}: {result:?}"
            );
            assert!(
                result
                    .candidates
                    .iter()
                    .all(|candidate| candidate.label != "rec"),
                "{input}: {result:?}"
            );
        }
    }

    #[test]
    fn complete_bare_expression_offers_input_locals_through_the_walk() {
        // End to end through `complete`: the identifier under the cursor
        // fuzzy-filters the walk's candidates, so a local bound earlier
        // in the input line is offered by prefix.
        let set = walk_set();
        let provider = AstScopeProvider::new(&set);
        let line = ".(local_x: .) { 1 + loc";
        let completion = complete(&set, &provider, line, line.len());
        assert!(
            completion.candidates.iter().any(|c| c.label == "local_x"),
            "got: {:?}",
            completion
                .candidates
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(completion.replace, (line.len() - 3)..line.len());
    }

    #[test]
    fn candidate_carries_match_indices() {
        let set = fn_set(&["add_i32"]);
        let prefix_indices = run(&set, ":t add", ":t add".len())
            .candidates
            .into_iter()
            .find(|c| c.label == "add_i32")
            .expect("prefix candidate present")
            .match_indices;
        assert_eq!(prefix_indices, vec![0, 1, 2]);
        let substring_indices = run(&set, ":t i32", ":t i32".len())
            .candidates
            .into_iter()
            .find(|c| c.label == "add_i32")
            .expect("substring candidate present")
            .match_indices;
        assert_eq!(substring_indices, vec![4, 5, 6]);
    }

    /// A name set of bare `fn`s named `names`, all in `demo/main`, for
    /// the fuzzy-ranking tests. Each item's FQN is `demo/main.<name>`.
    fn fn_set(names: &[&str]) -> NameSet {
        NameSet {
            items: names
                .iter()
                .map(|n| ItemEntry {
                    short_name: (*n).to_owned(),
                    fqn: format!("demo/main.{n}"),
                    kind: ItemKind::Fn,
                    source_module: "demo/main".to_owned(),
                    vis_pub: true,
                })
                .collect(),
            ..NameSet::default()
        }
    }

    #[test]
    fn fuzzy_score_tiers_are_ordered() {
        let exact = fuzzy_score("fac", "factorial").expect("prefix matches");
        let ci_prefix = fuzzy_score("FAC", "factorial").expect("ci-prefix matches");
        let substring = fuzzy_score("act", "factorial").expect("substring matches");
        let subsequence = fuzzy_score("ftl", "factorial").expect("subsequence matches");
        assert!(exact > ci_prefix);
        assert!(ci_prefix > substring);
        assert!(substring > subsequence);
        let longer = fuzzy_score("fact", "factorial").expect("longer prefix matches");
        assert!(longer > exact);
        assert_eq!(fuzzy_score("xyz", "factorial"), None);
    }

    #[test]
    fn fuzzy_match_finds_substring() {
        let set = fn_set(&["add_i32", "sub_i32"]);
        let vals = values(&set, ":t i32");
        assert!(vals.iter().any(|v| v == "add_i32"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "sub_i32"), "got: {vals:?}");
    }

    #[test]
    fn fuzzy_match_prefers_prefix() {
        let set = fn_set(&["manufacturer", "factor"]);
        let vals = values(&set, ":t fac");
        let factor = vals.iter().position(|v| v == "factor");
        let manufacturer = vals.iter().position(|v| v == "manufacturer");
        assert!(factor < manufacturer, "got: {vals:?}");
    }

    #[test]
    fn fuzzy_match_case_insensitive() {
        let set = NameSet {
            modules: vec![ModuleEntry {
                path: "demo/main".to_owned(),
                item_count: 0,
                import_count: 0,
                is_loaded: false,
            }],
            ..NameSet::default()
        };
        assert!(values(&set, ":load MAIN").iter().any(|v| v == "demo/main"));
    }

    #[test]
    fn fuzzy_match_subsequence_ranks_below_substring() {
        let set = fn_set(&["a_d_i_x", "radish"]);
        let vals = values(&set, ":t adi");
        let radish = vals
            .iter()
            .position(|v| v == "radish")
            .expect("substring present");
        let scattered = vals
            .iter()
            .position(|v| v == "a_d_i_x")
            .expect("subsequence present");
        assert!(radish < scattered, "got: {vals:?}");
    }

    #[test]
    fn fuzzy_match_subsequence_adjacency_breaks_ties() {
        assert!(
            fuzzy_score("abc", "abXc").expect("tight subsequence")
                > fuzzy_score("abc", "aXbXc").expect("loose subsequence")
        );
    }

    #[test]
    fn fuzzy_match_empty_query_offers_everything() {
        let set = fn_set(&["foo", "bar"]);
        let vals = values(&set, ":t ");
        assert!(vals.iter().any(|v| v == "foo"), "got: {vals:?}");
        assert!(vals.iter().any(|v| v == "bar"), "got: {vals:?}");
    }

    #[test]
    fn fuzzy_command_menu_matches_loosely() {
        let vals = values(&NameSet::default(), ":srce");
        assert!(vals.iter().any(|v| v == ":source"), "got: {vals:?}");
    }

    /// Stage `modules` into a loaded session (no disk / analysis) —
    /// enough for [`NameSet::from_session`], which reads the parsed ASTs.
    fn session_with(modules: &[(&str, &str)]) -> Session {
        use crate::repl_core::session::StagedModule;
        let mut s = Session::new(std::path::PathBuf::from("/tmp/pkg"));
        for (path, src) in modules {
            let module = crate::pass::parser::parse(src).expect("test module parses");
            let staged = StagedModule {
                path: (*path).to_owned(),
                file_path: std::path::PathBuf::from(format!("/tmp/{path}.kio")),
                module,
            };
            s.commit_load(path, &[staged]);
        }
        s
    }

    #[test]
    fn forwarding_completion_keeps_declaration_selectors_out_of_expression_names() {
        let source = concat!(
            "module pkg/api; fn field() -> . { () } ",
            "pub type {field} = {original};",
        );
        let session = session_with(&[("pkg/api", source)]);
        let mut names = NameSet::from_session(&session);
        names.current_module_src = Some(source.to_owned());
        assert_eq!(names.items.len(), 2);
        assert_eq!(names.items[1].short_name, "{field}");
        assert_eq!(names.items[1].fqn, "pkg/api.{field}");
        assert_eq!(names.items[1].kind, ItemKind::LabelForward);
        assert_eq!(names.items[1].kind.tag(), "label");
        for command in ["doc", "signature", "source", "refs", "which"] {
            let line = format!(":{command} {{fi");
            let completion = run(&names, &line, line.len());
            let candidate = completion
                .candidates
                .iter()
                .find(|candidate| candidate.label == "{field}")
                .unwrap();
            assert_eq!(candidate.kind, CandidateKind::Item(ItemKind::LabelForward));
            let mut accepted = line.clone();
            accepted.replace_range(completion.replace, &candidate.label);
            assert_eq!(accepted, format!(":{command} {{field}}"));
        }
        for line in ["fi", ":normalize fi", ":t fi", ":type fi", ":pure fi"] {
            let completion = run(&names, line, line.len());
            assert!(
                completion
                    .candidates
                    .iter()
                    .any(|candidate| candidate.label == "field")
            );
            assert!(
                completion.candidates.iter().all(|candidate| {
                    candidate.label != "Field" && !candidate.label.contains('{')
                })
            );
        }
        let ast = AstScopeProvider::new(&names);
        let candidates = ast.in_scope("fi", 2);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.label == "field")
        );
        assert!(
            candidates
                .iter()
                .all(|candidate| { candidate.label != "Field" && !candidate.label.contains('{') })
        );
    }

    #[test]
    fn complete_end_to_end_over_a_loaded_session() {
        let root = std::path::PathBuf::from("/kio-repl-tests/completion-exact");
        let files = std::collections::BTreeMap::from([
            (root.join("demo.pkg.kio"), "package demo; bridge { demo/**; }".to_owned()),
            (root.join("demo/main.kio"), "module demo/main; pub fn factorial(n: .) -> . { n } pub fn factory(n: .) -> . { n }".to_owned()),
            (root.join("demo/other.kio"), "module demo/other; import __intrinsics__; fn foreign_private() -> . { () }".to_owned()),
        ]);
        let mut session = Session::new_in_memory(root, files);
        for path in ["demo/other", "demo/main"] {
            let loaded = crate::repl_core::commands::Command::Load(path.to_owned())
                .run(&mut session, crate::repl_core::highlight::Palette::plain());
            assert!(
                loaded.output.contains(&format!("loaded {path}")),
                "{}",
                loaded.output
            );
        }
        let names = NameSet::from_session(&session);
        let provider = AstScopeProvider::new(&names);

        // Bare expression → in-scope identifiers.
        let bare = complete(&names, &provider, "fac", "fac".len());
        let bare_labels: Vec<&str> = bare.candidates.iter().map(|c| c.label.as_str()).collect();
        assert!(bare_labels.contains(&"factorial"), "got: {bare_labels:?}");
        assert!(bare_labels.contains(&"factory"), "got: {bare_labels:?}");
        assert!(
            bare.candidates
                .iter()
                .all(|c| matches!(c.kind, CandidateKind::Identifier(_))),
            "bare expression candidates are identifiers"
        );

        // `:t` argument → item name / FQN candidates.
        let typed = complete(&names, &provider, ":t fac", ":t fac".len());
        let typed_labels: Vec<&str> = typed.candidates.iter().map(|c| c.label.as_str()).collect();
        assert!(typed_labels.contains(&"factorial"), "got: {typed_labels:?}");
        assert!(
            typed_labels.contains(&"demo/main.factorial"),
            "got: {typed_labels:?}"
        );
        let scoped = provider.in_scope("foreign", 7);
        assert!(
            !scoped
                .iter()
                .any(|candidate| candidate.label == "foreign_private"
                    || candidate.label == "__pair__")
        );
        let browsed = complete(&names, &provider, ":source foreign", 15);
        assert!(
            browsed
                .candidates
                .iter()
                .any(|candidate| candidate.label == "demo/other.foreign_private")
        );
    }

    #[test]
    fn ast_scope_provider_uses_imported_operator_parse_context() {
        let root = std::path::PathBuf::from("/kio-repl-tests/completion");
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            root.join("app.pkg.kio"),
            "package app;\n\nbridge {\n  app/**;\n}\n".to_owned(),
        );
        files.insert(
            root.join("app/syntax.kio"),
            "module app/syntax;\npub fn choose(left: ., right: .) -> . { left }\npub op ? _ : _ { impl choose }\n"
                .to_owned(),
        );
        files.insert(
            root.join("app/main.kio"),
            "module app/main;\nimport app/syntax(op ? _ : _);\npub fn factorial(value: .) -> . { value }\nfn run() -> . { ? () : () }\n"
                .to_owned(),
        );
        let mut session = Session::new_in_memory(root, files);
        let loaded = crate::repl_core::commands::Command::Load("app/main".to_owned())
            .run(&mut session, crate::repl_core::highlight::Palette::plain());
        assert!(
            loaded.output.contains("loaded app/main"),
            "{}",
            loaded.output
        );
        let names = NameSet::from_session(&session);
        let provider = AstScopeProvider::new(&names);

        let input = ".(local: .) { ? () : loc";
        let scope = provider.in_scope(input, input.len());
        assert!(
            scope.iter().any(|candidate| candidate.label == "local"),
            "got: {scope:?}"
        );
    }

    #[test]
    fn collect_item_builds_operator_entry_not_item_entry() {
        // An `op _ + __ { impl add }` produces an OperatorEntry keyed on the
        // token spelling and binding `add` — never an ItemEntry.
        let module = crate::pass::parser::parse(
            "module pkg/m;\nfn add(x: ., y: .) -> . { x }\nop _ + __ { impl add }\n",
        )
        .expect("module parses");
        let mut items = Vec::new();
        let mut operators = Vec::new();
        for item in &module.items {
            collect_item("pkg/m", item, &mut items, &mut operators);
        }
        assert!(items.iter().any(|i| i.short_name == "add"));
        assert!(!items.iter().any(|i| i.short_name == "+"));
        assert_eq!(operators.len(), 1);
        assert_eq!(operators[0].grammar, "op _ + __");
        assert_eq!(operators[0].signature, "op _ + __ { impl add }");
        assert_eq!(operators[0].source_module, "pkg/m");
    }

    #[test]
    fn collect_item_indexes_every_rec_member() {
        let module = crate::pass::parser::parse(
            "module pkg/m; \
             rec(loop) { \
               fn local(value: .) -> . { rec exported(value) }; \
               pub fn exported(value: .) -> . { rec local(value) } \
             }",
        )
        .expect("module parses");
        let mut items = Vec::new();
        let mut operators = Vec::new();
        collect_item("pkg/m", &module.items[0], &mut items, &mut operators);

        assert!(items.iter().any(|item| item.short_name == "local"));
        assert!(items.iter().any(|item| item.short_name == "exported"));
        assert!(operators.is_empty());
    }
}
