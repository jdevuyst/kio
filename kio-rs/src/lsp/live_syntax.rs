use std::path::Path;

/// Current source text paired with whether its full parse succeeded.
#[derive(Debug, Clone, Copy)]
pub struct LiveSyntaxSnapshot<'a> {
    pub source: &'a str,
    pub parsed: bool,
}

pub fn snapshot_for<'a>(
    path: &Path,
    analysis: &'a crate::cmd::check::LspAnalysis,
    overlay: Option<LiveSyntaxSnapshot<'a>>,
) -> Option<LiveSyntaxSnapshot<'a>> {
    overlay.or_else(|| {
        analysis.sources.get(path).map(|source| LiveSyntaxSnapshot {
            source,
            parsed: true,
        })
    })
}

/// Typed positions are usable for the analyzed source or a parsed overlay.
pub fn typed_positions_are_current(
    path: &Path,
    analysis: &crate::cmd::check::LspAnalysis,
    snapshot: LiveSyntaxSnapshot<'_>,
) -> bool {
    analysis
        .sources
        .get(path)
        .is_some_and(|source| source == snapshot.source)
        || snapshot.parsed
}
