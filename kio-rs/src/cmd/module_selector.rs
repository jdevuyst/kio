//! Shared parser for the **optional positional module-selector**
//! that `kio test` and `kio doc {check,build}` accept
//! per [`specs/cli.md`](../../../specs/cli.md).
//!
//! Each subcommand's CLI takes zero or more positional selectors;
//! with none, the subcommand operates over its whole natural set
//! (every equiv-bearing module, every markdown file).
//! With one or more, the subcommand operates over the subset the
//! selectors name.
//!
//! A selector is one of:
//!
//! - A **module name** in module-path form (`pkg/utils/string`).
//!   Module paths share the `/` separator with filesystem paths, so
//!   the dispatch can't key on `/` alone; a selector is a module
//!   name when it neither ends in `.kio` nor carries a filesystem
//!   lead-in (an absolute path, a `\` separator, or a `./` / `../`
//!   relative prefix).
//! - A **filename path** — anything ending in `.kio`, anything
//!   containing a `\` separator, an absolute path, or a `./` / `../`
//!   relative-path lead-in. Resolved against the process `cwd` for
//!   absolute matching against module file paths.
//!
//! The dispatch is purely syntactic — no I/O happens here. The
//! caller is responsible for matching the parsed selectors against
//! its natural set and reporting unknown ones.

use std::path::PathBuf;

/// Render a module-map key for diagnostics and listings.
pub fn key_to_surface(key: &str) -> String {
    key.to_owned()
}

/// One parsed selector. Modules are stored by their textual name
/// (`/`-separated module path); paths are pre-canonicalised so
/// per-call comparisons against module file paths are O(1) hash
/// lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// `pkg/utils/string` — a module-path selector. Matched
    /// against the package's `Package::modules()` keys.
    Module(String),
    /// `src/main.kio` (or any other path-shaped argument). Stored
    /// as the canonicalised absolute path when canonicalisation
    /// succeeds; otherwise the verbatim path joined to `cwd`.
    /// Either way the caller compares it against the file paths
    /// the package's modules carry.
    Path(PathBuf),
}

/// Classify one raw CLI argument as a module name or a filename
/// path. The dispatch is by lexical shape only — see the module
/// docstring for the precise rule.
///
/// `cwd` is the resolution base for relative path-shaped selectors.
pub fn parse(arg: &str, cwd: &std::path::Path) -> Selector {
    // Module paths and filesystem paths both use `/`, so `/` alone
    // can't decide. A `.kio` suffix, a `\` separator, an absolute
    // path, or a `./` / `../` relative-path lead-in marks a filename;
    // everything else is a module path.
    let is_filename = crate::file_kind::has_kio_extension(arg)
        || arg.contains('\\')
        || arg.starts_with('/')
        || arg.starts_with("./")
        || arg.starts_with("../");
    if is_filename {
        let raw = PathBuf::from(arg);
        let joined = if raw.is_absolute() {
            raw
        } else {
            cwd.join(raw)
        };
        // Canonicalise when possible so the file path matches the
        // package's module file paths. A missing file falls back to the
        // joined path; the caller reports "unknown selector"
        // against its natural set.
        let canonical = std::fs::canonicalize(&joined).unwrap_or(joined);
        Selector::Path(canonical)
    } else {
        Selector::Module(arg.to_owned())
    }
}

/// An unknown selector was named on the CLI. The diagnostic (naming
/// the available modules) has already been printed to stderr by
/// [`resolve_against_modules`]; the caller maps this to the CLI-usage
/// exit code (2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownSelector;

/// Resolve each [`Selector`] against the available module set,
/// returning the set of matching slash module paths. `available` is
/// the package's `(module-path, file-path)` list; the caller builds it
/// from `Package::modules()`. An unknown selector is reported to
/// stderr — naming the available modules — and returned as
/// `Err(`[`UnknownSelector`]`)`, which the caller maps to the
/// CLI-usage exit code (2).
///
/// Shared by `kio check` and `kio test` so their positional-selector
/// surface stays identical (per [`specs/cli.md`](../../../specs/cli.md)).
pub fn resolve_against_modules(
    available: &[(String, std::path::PathBuf)],
    selectors: &[Selector],
) -> Result<std::collections::BTreeSet<String>, UnknownSelector> {
    // Stable order for the error diagnostic; matches module-path
    // order, which the user sees in other diagnostics too.
    let mut available: Vec<(String, std::path::PathBuf)> = available.to_vec();
    available.sort_by(|a, b| a.0.cmp(&b.0));

    let mut matched: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for sel in selectors {
        match sel {
            Selector::Module(name) => {
                if let Some((mp, _)) = available.iter().find(|(mp, _)| mp == name) {
                    matched.insert(mp.clone());
                } else {
                    eprintln!(
                        "error: no module named `{}` in the current directory",
                        key_to_surface(name)
                    );
                    eprintln_available_modules(&available);
                    return Err(UnknownSelector);
                }
            }
            Selector::Path(p) => {
                if let Some((mp, _)) = available.iter().find(|(_, fp)| same_path(fp, p)) {
                    matched.insert(mp.clone());
                } else {
                    eprintln!(
                        "error: no module file at `{}` in the current directory",
                        p.display()
                    );
                    eprintln_available_modules(&available);
                    return Err(UnknownSelector);
                }
            }
        }
    }
    Ok(matched)
}

/// Best-effort path equality: compare canonicalised forms when both
/// resolve, fall back to a raw byte equality otherwise.
fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Print the package's available module list to stderr, one per
/// line. Helps users who misspelled a selector see what's actually
/// in scope.
fn eprintln_available_modules(available: &[(String, std::path::PathBuf)]) {
    eprintln!("available modules:");
    for (mp, _) in available {
        eprintln!("  {}", key_to_surface(mp));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_name_parses_as_module() {
        let cwd = std::env::temp_dir();
        assert_eq!(
            parse("pkg/utils/string", &cwd),
            Selector::Module("pkg/utils/string".to_owned())
        );
        assert_eq!(parse("op", &cwd), Selector::Module("op".to_owned()));
    }

    #[test]
    fn dotted_name_is_not_rewritten_to_slash_module_key() {
        let cwd = std::env::temp_dir();
        assert_eq!(
            parse("pkg.utils.string", &cwd),
            Selector::Module("pkg.utils.string".to_owned())
        );
    }

    #[test]
    fn path_with_slash_parses_as_path() {
        let cwd = std::env::temp_dir();
        match parse("src/main.kio", &cwd) {
            Selector::Path(_) => {}
            other => panic!("expected Path, got {other:?}"),
        }
        match parse("./src/main.kio", &cwd) {
            Selector::Path(_) => {}
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn dot_kio_suffix_parses_as_path() {
        // Even without a `/`, a `.kio` suffix forces the path
        // interpretation — a single-file argument like `main.kio`
        // is a filename, not a module called `main` with a `.kio`
        // suffix.
        let cwd = std::env::temp_dir();
        match parse("main.kio", &cwd) {
            Selector::Path(_) => {}
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn absolute_path_parses_as_path() {
        let cwd = std::env::temp_dir();
        match parse("/tmp/foo.kio", &cwd) {
            Selector::Path(_) => {}
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn resolve_against_modules_matches_known_module_name() {
        let available = vec![
            ("a".to_owned(), std::path::PathBuf::from("/pkg/a.kio")),
            ("b".to_owned(), std::path::PathBuf::from("/pkg/b.kio")),
        ];
        let matched =
            resolve_against_modules(&available, &[Selector::Module("a".to_owned())]).unwrap();
        assert!(matched.contains("a"));
        assert!(!matched.contains("b"));
    }

    #[test]
    fn resolve_against_modules_rejects_unknown_module_name() {
        let available = vec![("a".to_owned(), std::path::PathBuf::from("/pkg/a.kio"))];
        assert!(
            resolve_against_modules(&available, &[Selector::Module("nope".to_owned())]).is_err()
        );
    }
}
