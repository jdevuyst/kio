//! Top-level error type for the compiler.
//!
//! One enum, one variant per pipeline phase. Each variant carries a
//! [`Diagnostic`] — enough information to map to an [`ExitCode`] from
//! `specs/exit-codes.md` and to render the structured diagnostic
//! specified in `specs/diagnostics.md`.
//!
//! Every variant carries the same payload: a mandatory `{span, message}`
//! pair plus the optional enrichments ([`Diagnostic::secondary`],
//! [`Diagnostic::help`], [`Diagnostic::notes`],
//! [`Diagnostic::fixes`]). The enrichments default empty, so a
//! construction site that supplies only `span` and `message` — via the
//! per-variant constructors ([`Error::type_`], [`Error::parse`], …) —
//! produces the floor-level `{span, message}` diagnostic Kio emitted
//! before the enriched contract landed. The chainable
//! `with_secondary` / `with_help` / `with_note` / `with_fix`
//! builders layer the optional fields on at the construction site.
//!
//! This type is the **user-error** channel only. An internal contract
//! the implementation believes unreachable panics (mapping to the
//! internal-error exit code); it is never wrapped in an [`Error`] and
//! rendered as a span-bearing diagnostic. See `specs/diagnostics.md`.

use std::path::PathBuf;

use crate::exit_code::ExitCode;
use crate::span::Span;

/// One text edit in a diagnostic fix. `file = None` means the
/// diagnostic's own file; `Some(path)` targets another source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixEdit {
    pub file: Option<PathBuf>,
    pub span: Span,
    pub replacement: String,
    /// A compiler-authored replacement assembled from literal text and exact
    /// source slices. This is used for structural repairs which must preserve
    /// declarations byte-for-byte. An empty list means [`Self::replacement`]
    /// is the complete replacement.
    pub replacement_parts: Vec<FixReplacementPart>,
    /// Source ranges which must contain whitespace only before this edit is
    /// materialized. Structural fixes use these guards to withhold an action
    /// when comments, visibility, or other unowned text sits between the
    /// compiler-selected declaration spans.
    pub required_whitespace: Vec<Span>,
}

/// One part of a source-preserving diagnostic repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixReplacementPart {
    Text(String),
    Source(Span),
}

/// One secondary diagnostic location.
///
/// `file = None` means the diagnostic's primary file. `Some(file)` is the
/// exact related source selected by the semantic producer; renderers must
/// never interpret that span against the primary source bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryLabel {
    pub file: Option<PathBuf>,
    pub span: Span,
    pub text: String,
}

impl SecondaryLabel {
    pub fn same_file(span: Span, text: impl Into<String>) -> Self {
        Self {
            file: None,
            span,
            text: text.into(),
        }
    }

    pub fn in_file(file: impl Into<PathBuf>, span: Span, text: impl Into<String>) -> Self {
        Self {
            file: Some(file.into()),
            span,
            text: text.into(),
        }
    }
}

impl FixEdit {
    pub fn new(span: Span, replacement: impl Into<String>) -> Self {
        Self {
            file: None,
            span,
            replacement: replacement.into(),
            replacement_parts: Vec::new(),
            required_whitespace: Vec::new(),
        }
    }

    /// Replace `span` with the concatenation of compiler-selected literals and
    /// exact slices of the diagnostic's own source file. The editor merely
    /// materializes these spans against the authenticated document version;
    /// it does not infer declaration identity or dependency structure.
    pub fn from_parts(
        span: Span,
        replacement_parts: Vec<FixReplacementPart>,
        required_whitespace: Vec<Span>,
    ) -> Self {
        Self {
            file: None,
            span,
            replacement: String::new(),
            replacement_parts,
            required_whitespace,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    MachineApplicable,
    MaybeIncorrect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub title: String,
    pub edits: Vec<FixEdit>,
    pub applicability: Applicability,
    /// An insertion scaffold requires user-authored content; it is not a
    /// claim that applying the edit repairs the program's type errors.
    pub scaffold: bool,
    /// A semantic source scope that the compiler has validated independently
    /// of later declarations. When set, an editor recheck may still publish
    /// this fix if repairing the scope exposes a different first error wholly
    /// outside it. This supports honest sequential repairs without weakening
    /// the ordinary whole-document validation policy.
    pub follow_on_reanalysis_scope: Option<Span>,
}

impl Fix {
    pub fn machine_applicable(title: impl Into<String>, edits: Vec<FixEdit>) -> Self {
        Self {
            title: title.into(),
            edits,
            applicability: Applicability::MachineApplicable,
            scaffold: false,
            follow_on_reanalysis_scope: None,
        }
    }

    pub fn maybe_incorrect(title: impl Into<String>, edits: Vec<FixEdit>) -> Self {
        Self {
            title: title.into(),
            edits,
            applicability: Applicability::MaybeIncorrect,
            scaffold: false,
            follow_on_reanalysis_scope: None,
        }
    }

    /// Permit a later, unchanged source region to surface its own first error
    /// after this compiler-validated scope is repaired.
    pub fn allowing_follow_on_reanalysis_outside(mut self, scope: Span) -> Self {
        self.follow_on_reanalysis_scope = Some(scope);
        self
    }

    pub fn scaffold(title: impl Into<String>, edits: Vec<FixEdit>) -> Self {
        Self {
            scaffold: true,
            ..Self::maybe_incorrect(title, edits)
        }
    }
}

/// The optional enrichments of a [`Diagnostic`], boxed off the hot
/// path. The overwhelming majority of diagnostics are floor-level
/// (`{span, message}` only); keeping these fields behind an
/// `Option<Box<_>>` on [`Diagnostic`] keeps the common case small, so a
/// `Result<_, Error>` stays cheap to move and clippy's `result_large_err`
/// stays quiet. Allocated lazily by the `with_*` builders the first time
/// an enrichment is attached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagExtra {
    /// An established primary file carried through nested, unlocated checking.
    /// Located-error collection consumes this field into its primary file.
    pub source_file: Option<PathBuf>,
    /// The other load-bearing locations: the binding site, the
    /// expected-type source, the conflicting declaration. Each carries an
    /// optional exact file, a span, and the label drawn under it.
    pub secondary: Vec<SecondaryLabel>,
    /// A concrete proposed fix, phrased as guidance.
    pub help: Option<String>,
    /// Clarifying context not tied to a span.
    pub notes: Vec<String>,
    /// Machine-readable fixes. Each fix may contain one or more edits.
    pub fixes: Vec<Fix>,
    /// The unresolved value/type name for lazy editor actions.
    pub unresolved_name: Option<String>,
}

/// The diagnostic payload shared by every [`Error`] variant.
///
/// `span` + `message` are mandatory; the [`DiagExtra`] enrichments
/// specified in `specs/diagnostics.md` are optional and default absent.
/// Holding the payload in one struct (rather than spreading the fields
/// across each variant's literal) is what lets the per-variant
/// constructors stay a single `{span, message}` call while the
/// enrichments remain reachable through the chainable builders on
/// [`Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The offending location — where the caret points.
    pub span: Span,
    /// Names the problem in the user's surface vocabulary.
    pub message: String,
    /// The optional enrichments, boxed and absent for a floor-level
    /// diagnostic. Reach the individual fields through
    /// [`Diagnostic::secondary`], [`Diagnostic::help`],
    /// [`Diagnostic::notes`], [`Diagnostic::fixes`].
    pub extra: Option<Box<DiagExtra>>,
}

impl Diagnostic {
    /// A floor-level diagnostic: the mandatory `{span, message}` pair
    /// with no enrichment.
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            extra: None,
        }
    }

    /// The secondary labels, empty when none are attached.
    pub fn secondary(&self) -> &[SecondaryLabel] {
        self.extra.as_ref().map_or(&[], |e| &e.secondary)
    }

    /// The `help:` line, if any.
    pub fn help(&self) -> Option<&str> {
        self.extra.as_ref().and_then(|e| e.help.as_deref())
    }

    /// The `note:` lines, empty when none are attached.
    pub fn notes(&self) -> &[String] {
        self.extra.as_ref().map_or(&[], |e| &e.notes)
    }

    /// Machine-readable fixes, empty when none are attached.
    pub fn fixes(&self) -> &[Fix] {
        self.extra.as_ref().map_or(&[], |e| e.fixes.as_slice())
    }

    pub fn unresolved_name(&self) -> Option<&str> {
        self.extra
            .as_ref()
            .and_then(|e| e.unresolved_name.as_deref())
    }

    /// The enrichment block, allocating an empty one on first touch.
    fn extra_mut(&mut self) -> &mut DiagExtra {
        self.extra.get_or_insert_with(Box::default)
    }
}

/// The compiler's top-level user-error type. One variant per pipeline
/// phase; each carries a [`Diagnostic`] and maps to an [`ExitCode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Lex / parse error — exit code 11.
    Parse(Diagnostic),
    /// Use-statement error (unknown module, missing pub item, cycle, etc.)
    /// — exit code 12.
    Import(Diagnostic),
    /// Name-resolution error (unbound, duplicate, shadowing) — exit code 13.
    NameRes(Diagnostic),
    /// Type error (mismatch, arity, scheme-in-monomorphic-position) — exit
    /// code 14.
    Type(Diagnostic),
    /// Elaborator error: `into!` has no valid coercion, or `onto!`
    /// has no valid coercion — exit code 15. The structural-coercion
    /// rules give the elaborator a fixed search space; failure here
    /// means the user asked for a coercion that the rules can't
    /// produce, not that types are ill-formed.
    Elaborator(Diagnostic),
    /// Totality error (strict-positivity violation in a `newtype`
    /// payload, or a `loop` reached through an arrow left-hand side) —
    /// exit code 16. The language's totality contract is what keeps
    /// Kio' strongly normalizing. `match!` non-exhaustiveness and
    /// unreachable clauses surface as elaborator errors (code 15),
    /// not here.
    Totality(Diagnostic),
    /// Bridge-contract or signature-changelog error (dead bridge glob,
    /// incomplete bridged module set, unexposed signature type, invalid
    /// changelog transition, etc.) — exit code 20.
    Bridge(Diagnostic),
    /// Dependency / resolution error (malformed `*.dep.kio` /
    /// `*.lock.kio`, unresolvable or unobtainable dependency,
    /// dependency-name / local-root collision, a tampered upgrade,
    /// etc.) — exit code 30.
    Dep(Diagnostic),
    /// An environment / I/O failure the implementation did not plan for
    /// (a package-root canonicalize failure, an unreadable `.kio` file
    /// mid-walk) — exit code 1. Not a user-content error.
    Internal(Diagnostic),
}

impl Error {
    #[cfg(feature = "surface")]
    pub(crate) fn with_source_file(mut self, file: impl Into<PathBuf>) -> Self {
        if self.source_file().is_none() {
            let file = file.into();
            if !file.as_os_str().is_empty() {
                self.diagnostic_mut().extra_mut().source_file = Some(file);
            }
        }
        self
    }

    #[cfg(feature = "surface")]
    pub(crate) fn source_file(&self) -> Option<&std::path::Path> {
        self.diagnostic()
            .extra
            .as_ref()
            .and_then(|extra| extra.source_file.as_deref())
    }

    pub(crate) fn take_source_file(&mut self) -> Option<PathBuf> {
        self.diagnostic_mut()
            .extra
            .as_mut()
            .and_then(|extra| extra.source_file.take())
    }

    pub fn exit_code(&self) -> ExitCode {
        match self {
            Error::Parse(_) => ExitCode::Parse,
            Error::Import(_) => ExitCode::Import,
            Error::NameRes(_) => ExitCode::NameRes,
            Error::Type(_) => ExitCode::Type,
            Error::Elaborator(_) => ExitCode::Elaborator,
            Error::Totality(_) => ExitCode::Totality,
            Error::Bridge(_) => ExitCode::Bridge,
            Error::Dep(_) => ExitCode::Dep,
            Error::Internal(_) => ExitCode::Internal,
        }
    }

    /// The variant's [`Diagnostic`] payload.
    pub fn diagnostic(&self) -> &Diagnostic {
        match self {
            Error::Parse(d)
            | Error::Import(d)
            | Error::NameRes(d)
            | Error::Type(d)
            | Error::Elaborator(d)
            | Error::Totality(d)
            | Error::Bridge(d)
            | Error::Dep(d)
            | Error::Internal(d) => d,
        }
    }

    fn diagnostic_mut(&mut self) -> &mut Diagnostic {
        match self {
            Error::Parse(d)
            | Error::Import(d)
            | Error::NameRes(d)
            | Error::Type(d)
            | Error::Elaborator(d)
            | Error::Totality(d)
            | Error::Bridge(d)
            | Error::Dep(d)
            | Error::Internal(d) => d,
        }
    }

    /// Primary span and message for diagnostic rendering.
    ///
    /// The floor-level pair every Kio diagnostic carries. Callers that
    /// need the enrichments reach for [`Error::diagnostic`].
    pub fn diag(&self) -> (Span, &str) {
        let d = self.diagnostic();
        (d.span, d.message.as_str())
    }

    /// Add a secondary label pointing at another load-bearing span.
    /// Chainable; appends to any labels already present.
    pub fn with_secondary(mut self, span: Span, label: impl Into<String>) -> Self {
        self.diagnostic_mut()
            .extra_mut()
            .secondary
            .push(SecondaryLabel::same_file(span, label));
        self
    }

    /// Add a secondary label in one exact related source file. This performs
    /// no filesystem lookup or path recovery; the caller supplies declaration
    /// identity already selected by semantic analysis.
    pub fn with_secondary_in_file(
        mut self,
        file: impl Into<PathBuf>,
        span: Span,
        label: impl Into<String>,
    ) -> Self {
        self.diagnostic_mut()
            .extra_mut()
            .secondary
            .push(SecondaryLabel::in_file(file, span, label));
        self
    }

    /// Attach a `help:` line proposing a concrete fix. Chainable.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.diagnostic_mut().extra_mut().help = Some(help.into());
        self
    }

    /// Append a `note:` line of span-free clarifying context. Chainable.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.diagnostic_mut().extra_mut().notes.push(note.into());
        self
    }

    /// Re-anchor a diagnostic at another source file's call site. Source-bound
    /// enrichments cannot be rendered against that file, so only the category
    /// and message survive. Chainable.
    #[cfg(feature = "surface")]
    pub(crate) fn reanchored_at(mut self, span: Span) -> Self {
        let diagnostic = self.diagnostic_mut();
        diagnostic.span = span;
        diagnostic.extra = None;
        self
    }

    /// Attach a machine-applicable suggestion (the `did you mean …?`
    /// substrate). Chainable; appends a single-edit fix.
    pub fn with_suggestion(mut self, span: Span, replacement: impl Into<String>) -> Self {
        let replacement = replacement.into();
        self.diagnostic_mut()
            .extra_mut()
            .fixes
            .push(Fix::machine_applicable(
                format!("Replace with `{replacement}`"),
                vec![FixEdit::new(span, replacement)],
            ));
        self
    }

    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.diagnostic_mut().extra_mut().fixes.push(fix);
        self
    }

    pub fn with_unresolved_name(mut self, name: impl Into<String>) -> Self {
        self.diagnostic_mut().extra_mut().unresolved_name = Some(name.into());
        self
    }
}

/// Per-variant constructors from the floor-level `{span, message}` pair.
///
/// The pipeline's ~225 construction sites build a diagnostic from a span
/// and a message; these constructors are the single-call entry point so
/// a site reads `Error::type_(span, message)` and the enrichment fields
/// fill in their empty defaults. Enrichment rides on top via the
/// chainable `with_*` builders.
impl Error {
    pub fn parse(span: Span, message: impl Into<String>) -> Self {
        Error::Parse(Diagnostic::new(span, message))
    }
    pub fn import(span: Span, message: impl Into<String>) -> Self {
        Error::Import(Diagnostic::new(span, message))
    }
    pub fn name_res(span: Span, message: impl Into<String>) -> Self {
        Error::NameRes(Diagnostic::new(span, message))
    }
    pub fn type_(span: Span, message: impl Into<String>) -> Self {
        Error::Type(Diagnostic::new(span, message))
    }
    pub fn elaborator(span: Span, message: impl Into<String>) -> Self {
        Error::Elaborator(Diagnostic::new(span, message))
    }
    pub fn totality(span: Span, message: impl Into<String>) -> Self {
        Error::Totality(Diagnostic::new(span, message))
    }
    pub fn bridge(span: Span, message: impl Into<String>) -> Self {
        Error::Bridge(Diagnostic::new(span, message))
    }
    pub fn dep(span: Span, message: impl Into<String>) -> Self {
        Error::Dep(Diagnostic::new(span, message))
    }
    pub fn internal(span: Span, message: impl Into<String>) -> Self {
        Error::Internal(Diagnostic::new(span, message))
    }
}

/// The maximum Levenshtein distance a candidate may be from the
/// reference name and still be offered as a `did you mean …?`
/// suggestion. Two edits catches the common typo classes (a swapped,
/// dropped, doubled, or mistyped pair of characters) without proposing
/// an unrelated name on a long identifier.
const SUGGEST_THRESHOLD: usize = 2;

/// Find the closest name in `candidates` to `name` by Levenshtein
/// distance, returning it only when within [`SUGGEST_THRESHOLD`] — the
/// substrate behind every `did you mean …?` suggestion. `None` when no
/// candidate is close enough, so a genuinely novel typo stays a plain
/// unbound-name error rather than proposing a misleading near-miss.
///
/// Ties resolve to the first candidate at the best distance; callers
/// pass the in-scope set in a deterministic order (sorted) so the
/// suggestion is stable across runs.
pub fn closest_name<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let mut best: Option<(&str, usize)> = None;
    for candidate in candidates {
        // A candidate identical to the reference isn't a useful
        // suggestion (the name is in scope under a different lookup
        // path, or the caller already handled the exact match).
        if candidate == name {
            continue;
        }
        let dist = edit_distance(name, candidate);
        if dist <= SUGGEST_THRESHOLD && best.is_none_or(|(_, prev)| dist < prev) {
            best = Some((candidate, dist));
        }
    }
    best.map(|(s, _)| s)
}

/// Levenshtein edit distance between two strings, counted in Unicode
/// scalar values. The kernel behind [`closest_name`].
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    // Two rolling rows of the DP table — the full matrix is never needed.
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

#[cfg(test)]
mod suggest_tests {
    use super::*;

    #[test]
    fn secondary_label_builders_preserve_primary_and_exact_file_identity() {
        let primary = Span::new(2, 3);
        let same_file = Span::new(5, 7);
        let provider = Span::new(11, 13);
        let error = Error::type_(primary, "invalid placeholder")
            .with_secondary(same_file, "same-file binder")
            .with_secondary_in_file("provider.kio", provider, "provider binder")
            .with_help("write a concrete type");

        let secondary = error.diagnostic().secondary();
        assert_eq!(secondary.len(), 2);
        assert_eq!(secondary[0].file, None);
        assert_eq!(secondary[0].span, same_file);
        assert_eq!(secondary[0].text, "same-file binder");
        assert_eq!(
            secondary[1].file.as_deref(),
            Some(std::path::Path::new("provider.kio"))
        );
        assert_eq!(secondary[1].span, provider);
        assert_eq!(secondary[1].text, "provider binder");
        assert_eq!(error.diag(), (primary, "invalid placeholder"));
        assert_eq!(error.diagnostic().help(), Some("write a concrete type"));
        assert_eq!(error.clone(), error);
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("foo", "foo"), 0);
        assert_eq!(edit_distance("foo", "fao"), 1);
        assert_eq!(edit_distance("foo", "bar"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
    }

    #[test]
    fn closest_name_within_threshold() {
        let cands = ["greet", "main", "helper"];
        assert_eq!(closest_name("gret", cands), Some("greet"));
        assert_eq!(closest_name("mian", cands), Some("main"));
    }

    #[test]
    fn closest_name_rejects_far_and_exact() {
        let cands = ["greet", "main"];
        assert_eq!(closest_name("xyzzy", cands), None);
        // An exact match is not offered as a suggestion.
        assert_eq!(closest_name("main", cands), None);
    }

    #[test]
    fn closest_name_picks_nearest() {
        let cands = ["abcd", "abce"];
        // `abcf` is distance 1 from both; the first at the best
        // distance wins.
        assert_eq!(closest_name("abcf", cands), Some("abcd"));
    }
}
