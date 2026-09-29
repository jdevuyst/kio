use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::ast::BuildBlockCache;
use crate::package_collection::{PackageKey, ParsedPackage, ParsedPackageCollection};

#[derive(Debug, Clone)]
pub struct SemanticCacheRoots {
    roots: BTreeMap<PackageKey, PathBuf>,
}

impl SemanticCacheRoots {
    pub fn empty() -> Self {
        Self {
            roots: BTreeMap::new(),
        }
    }

    pub fn compute(parsed_ws: &ParsedPackageCollection) -> Self {
        let mut roots = BTreeMap::new();
        for (key, parsed_pkg) in &parsed_ws.packages {
            if let Some(root) = explicit_cache_root(parsed_pkg) {
                roots.insert(key.clone(), root);
            }
        }
        Self { roots }
    }

    pub fn root_for(&self, key: &PackageKey) -> Option<&Path> {
        self.roots.get(key).map(PathBuf::as_path)
    }
}

fn explicit_cache_root(parsed_pkg: &ParsedPackage) -> Option<PathBuf> {
    let build = parsed_pkg
        .package_file
        .as_ref()?
        .package_file
        .build
        .as_ref()?;
    match &build.cache {
        BuildBlockCache::Path { path, .. } => Some(
            crate::cache::package_check::resolve_cache_root(&parsed_pkg.root_dir, path),
        ),
        BuildBlockCache::Disabled { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Meta, PackageFile, Surface};
    use crate::pass::resolve::PackageFileEntry;
    use crate::span::Span;

    fn key(root: &str, name: &str) -> PackageKey {
        PackageKey {
            canonical_dir: PathBuf::from(root),
            package_name: name.to_owned(),
        }
    }

    fn package(root: &str, name: &str, cache: Option<Option<&str>>) -> ParsedPackage {
        ParsedPackage {
            root_dir: PathBuf::from(root),
            modules: Vec::new(),
            lazy_modules: BTreeMap::new(),
            package_file: Some(PackageFileEntry {
                file_path: PathBuf::from(root).join(format!("{name}.pkg.kio")),
                package_name: name.to_owned(),
                package_file: PackageFile::<Surface> {
                    name: name.to_owned(),
                    build: cache.map(|cache| crate::ast::BuildBlock {
                        leading_trivia: Vec::new(),
                        trailing_trivia: Vec::new(),
                        cache: match cache {
                            Some(path) => BuildBlockCache::Path {
                                path: path.to_owned(),
                                span: Span::new(0, 0),
                                leading_trivia: Vec::new(),
                            },
                            None => BuildBlockCache::Disabled {
                                span: Span::new(0, 0),
                                leading_trivia: Vec::new(),
                            },
                        },
                        targets: Vec::new(),
                        docs: None,
                        span: Span::new(0, 0),
                    }),
                    bridge: None,
                    meta: Meta::new(Span::new(0, 0)),
                },
            }),
            dep_files: Vec::new(),
            sources: BTreeMap::new(),
        }
    }

    fn workspace(root_pkg: ParsedPackage) -> ParsedPackageCollection {
        let root = key("/root", "app");
        let mut packages = BTreeMap::new();
        packages.insert(root.clone(), root_pkg);
        ParsedPackageCollection { root, packages }
    }

    #[test]
    fn explicit_package_cache_root_is_recorded() {
        let root_key = key("/root", "app");
        let ws = workspace(package("/root", "app", Some(Some("out/cache"))));
        let roots = SemanticCacheRoots::compute(&ws);
        assert_eq!(
            roots.root_for(&root_key),
            Some(Path::new("/root").join("out/cache").as_path())
        );
    }

    #[test]
    fn absent_build_block_has_no_cache_root() {
        let root_key = key("/root", "app");
        let ws = workspace(package("/root", "app", None));
        let roots = SemanticCacheRoots::compute(&ws);
        assert!(roots.root_for(&root_key).is_none());
    }

    #[test]
    fn disabled_cache_has_no_cache_root() {
        let root_key = key("/root", "app");
        let ws = workspace(package("/root", "app", Some(None)));
        let roots = SemanticCacheRoots::compute(&ws);
        assert!(roots.root_for(&root_key).is_none());
    }
}
