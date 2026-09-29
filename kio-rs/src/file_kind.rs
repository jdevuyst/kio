//! The one recognizer for what kind of `.kio` file a path names.
//!
//! Kio has several on-disk file kinds, all sharing the `.kio` extension
//! and distinguished only by their filename suffix:
//!
//! - `<name>.pkg.kio` — a package file (the boundary contract).
//! - `<name>.sig.kio` — a package-signature changelog (`kio sig`).
//! - `<local>.dep.kio` — a dependency declaration (one direct
//!   cross-package dependency, keyed by its local name).
//! - `<local>.lock.kio` — a dependency lock file (the committed pin of
//!   what a remote dependency's floating `ref` resolved to).
//! - `<path>.kio` — a regular module file (everything else).
//!
//! File-kind is decided ad hoc at many sites (the package walk, the
//! formatter, the Kiodoc renderer, the LSP, the REPL, …). Before the
//! signature file existed, "ends with `.kio` and not `.pkg.kio`" was a
//! safe proxy for "module file"; with `<name>.sig.kio` in the tree that
//! proxy is wrong — a sig file parses as a module and fails
//! path-coherence. Routing every site through this module keeps the
//! non-module suffixes recognized in exactly one place.

/// The `.pkg.kio` package-file suffix.
pub const PKG_KIO_SUFFIX: &str = ".pkg.kio";

/// The `.sig.kio` package-signature-changelog suffix.
pub const SIG_KIO_SUFFIX: &str = ".sig.kio";

/// The `.dep.kio` dependency-declaration suffix.
pub const DEP_KIO_SUFFIX: &str = ".dep.kio";

/// The `.lock.kio` dependency-lock suffix.
pub const LOCK_KIO_SUFFIX: &str = ".lock.kio";

/// The bare `.kio` extension every Kio file shares.
pub const KIO_SUFFIX: &str = ".kio";

/// True when `filename` names a `.pkg.kio` package file.
pub fn is_package_file(filename: &str) -> bool {
    filename.ends_with(PKG_KIO_SUFFIX)
}

/// True when `filename` names a `.sig.kio` package-signature changelog.
pub fn is_sig_file(filename: &str) -> bool {
    filename.ends_with(SIG_KIO_SUFFIX)
}

/// True when `filename` names a `.dep.kio` dependency declaration.
pub fn is_dep_file(filename: &str) -> bool {
    filename.ends_with(DEP_KIO_SUFFIX)
}

/// True when `filename` names a `.lock.kio` dependency lock file.
pub fn is_lock_file(filename: &str) -> bool {
    filename.ends_with(LOCK_KIO_SUFFIX)
}

/// True when `filename` names a regular module file — it carries the
/// `.kio` extension but is none of the file-shape kinds (package,
/// signature, dependency declaration, or dependency lock).
///
/// This is the predicate every module walk wants: a `.sig.kio`, a
/// `.dep.kio`, a `.lock.kio`, and a `.pkg.kio` are each *not* a module,
/// so a site that means "is this a module source file?" must exclude
/// every such suffix, not just `.pkg.kio`.
pub fn is_module_file(filename: &str) -> bool {
    filename.ends_with(KIO_SUFFIX)
        && !is_package_file(filename)
        && !is_sig_file(filename)
        && !is_dep_file(filename)
        && !is_lock_file(filename)
}

/// True when `filename` carries the `.kio` extension, of any kind. Used
/// for coarse path-arg validation where the kind is decided later.
pub fn has_kio_extension(filename: &str) -> bool {
    filename.ends_with(KIO_SUFFIX)
}

/// The stem of a `<name>.pkg.kio` filename (`name`), or `None` when the
/// filename is not a package file.
pub fn package_stem(filename: &str) -> Option<&str> {
    filename.strip_suffix(PKG_KIO_SUFFIX)
}

/// The stem of a `<name>.sig.kio` filename (`name`), or `None` when the
/// filename is not a signature file.
pub fn sig_stem(filename: &str) -> Option<&str> {
    filename.strip_suffix(SIG_KIO_SUFFIX)
}

/// The stem of a `<local>.dep.kio` filename (the dependency's local
/// name), or `None` when the filename is not a dependency declaration.
pub fn dep_stem(filename: &str) -> Option<&str> {
    filename.strip_suffix(DEP_KIO_SUFFIX)
}

/// The stem of a `<local>.lock.kio` filename (the dependency's local
/// name), or `None` when the filename is not a dependency lock file.
pub fn lock_stem(filename: &str) -> Option<&str> {
    filename.strip_suffix(LOCK_KIO_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_file_excludes_package_sig_dep_and_lock() {
        assert!(is_module_file("main.kio"));
        assert!(is_module_file("a/b/c.kio"));
        assert!(!is_module_file("app.pkg.kio"));
        assert!(!is_module_file("app.sig.kio"));
        assert!(!is_module_file("foobar.dep.kio"));
        assert!(!is_module_file("foobar.lock.kio"));
        assert!(!is_module_file("README.md"));
    }

    #[test]
    fn package_sig_dep_and_lock_are_distinct() {
        assert!(is_package_file("app.pkg.kio"));
        assert!(!is_package_file("app.sig.kio"));
        assert!(!is_package_file("foobar.dep.kio"));
        assert!(!is_package_file("foobar.lock.kio"));
        assert!(is_sig_file("app.sig.kio"));
        assert!(!is_sig_file("app.pkg.kio"));
        assert!(!is_sig_file("foobar.dep.kio"));
        assert!(is_dep_file("foobar.dep.kio"));
        assert!(!is_dep_file("app.pkg.kio"));
        assert!(!is_dep_file("app.sig.kio"));
        assert!(!is_dep_file("foobar.lock.kio"));
        assert!(is_lock_file("foobar.lock.kio"));
        assert!(!is_lock_file("foobar.dep.kio"));
        assert!(!is_lock_file("app.pkg.kio"));
        assert!(has_kio_extension("app.sig.kio"));
        assert!(has_kio_extension("app.pkg.kio"));
        assert!(has_kio_extension("foobar.dep.kio"));
        assert!(has_kio_extension("foobar.lock.kio"));
        assert!(has_kio_extension("app.kio"));
    }

    #[test]
    fn stems_strip_the_right_suffix() {
        assert_eq!(package_stem("app.pkg.kio"), Some("app"));
        assert_eq!(package_stem("app.sig.kio"), None);
        assert_eq!(sig_stem("app.sig.kio"), Some("app"));
        assert_eq!(sig_stem("app.pkg.kio"), None);
        assert_eq!(dep_stem("foobar.dep.kio"), Some("foobar"));
        assert_eq!(dep_stem("app.pkg.kio"), None);
        assert_eq!(dep_stem("app.sig.kio"), None);
        assert_eq!(lock_stem("foobar.lock.kio"), Some("foobar"));
        assert_eq!(lock_stem("foobar.dep.kio"), None);
        assert_eq!(lock_stem("app.pkg.kio"), None);
    }
}
