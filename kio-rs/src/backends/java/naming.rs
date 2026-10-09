//! Declaration-owned Java type paths, independent of namespace occupancy.

use crate::backends::boundary_facade::QualifiedTypeName;
use crate::backends::public_names::{FacadeSelector, encode_source_identity};
use crate::backends::skin::{budgeted_mint_name, fnv1a_64_hex};

pub(super) const NOMINAL_CLASSFILE_BUDGET: usize = 240;
const CLASSFILE_ENVELOPE: usize = "Shapes$$KioStorage.class".len();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NominalKind {
    Host,
    Newtype,
}

pub(super) fn shadows_support_package(name: &str) -> bool {
    // Qualified support references use this root in type and expression positions.
    matches!(name, "java")
}

impl NominalKind {
    fn name(self) -> &'static str {
        match self {
            Self::Host => "Host",
            Self::Newtype => "Newtype",
        }
    }
}

pub(super) fn nominal_path(name: &QualifiedTypeName, kind: NominalKind) -> Vec<String> {
    let mut path = Vec::new();
    for (depth, component) in name.module_segments().iter().enumerate() {
        let readable = format!("KioModule_{}", encode_source_identity(component));
        path.push(if contains_folded(&path, &readable) {
            format!("KioModule_D{depth}_{}", encode_source_identity(component))
        } else {
            readable
        });
    }
    let leaf = name.name();
    let folded = leaf.to_ascii_lowercase();
    path.push(
        if folded == "shapes"
            || contains_folded(&path, leaf)
            || ["kiomodule_", "kiotype_", "kionominal_"]
                .iter()
                .any(|prefix| folded.starts_with(prefix))
        {
            format!("KioType_{}", encode_source_identity(leaf))
        } else {
            leaf.to_owned()
        },
    );
    if path.iter().map(String::len).sum::<usize>() + path.len() - 1 + CLASSFILE_ENVELOPE
        > NOMINAL_CLASSFILE_BUDGET
    {
        let identity = format!(
            "{}:{}/{}",
            kind.name(),
            name.module_segments().join("/"),
            name.name()
        );
        let readable = format!("KioNominal_{}_{}", kind.name(), path.join("__"));
        let bounded = budgeted_mint_name(readable, |_| {
            Ok::<_, std::convert::Infallible>(format!(
                "KioNominal_{}_H{}",
                kind.name(),
                fnv1a_64_hex(&identity)
            ))
        })
        .unwrap_or_else(|never| match never {});
        path = vec![bounded];
    }
    path
}

fn contains_folded(path: &[String], name: &str) -> bool {
    path.iter()
        .any(|component| component.eq_ignore_ascii_case(name))
}

pub(super) fn facade_class_name(path: &[FacadeSelector], role: &str) -> String {
    let identity = path
        .iter()
        .map(|selector| {
            let role = match selector {
                FacadeSelector::Module(_) => "M",
                FacadeSelector::Type(_) => "T",
            };
            format!("{role}_{}", encode_source_identity(selector.source()))
        })
        .collect::<Vec<_>>()
        .join("__");
    let readable = format!("{role}_{identity}");
    budgeted_mint_name(readable, |identity| {
        Ok::<_, std::convert::Infallible>(format!("{role}_H{}", fnv1a_64_hex(identity)))
    })
    .unwrap_or_else(|never| match never {})
}

pub(super) fn nominal_name(name: &QualifiedTypeName, kind: NominalKind) -> String {
    nominal_path(name, kind).join(".")
}

pub(super) fn storage_name(name: &QualifiedTypeName) -> &'static str {
    if nominal_path(name, NominalKind::Host)
        .iter()
        .any(|component| component.eq_ignore_ascii_case("Storage"))
    {
        "KioStorage"
    } else {
        "Storage"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(module: &str, leaf: &str) -> QualifiedTypeName {
        QualifiedTypeName::new(
            module.split('/').map(str::to_owned).collect(),
            leaf.to_owned(),
        )
        .unwrap()
    }

    #[test]
    fn nominal_paths_preserve_roles_and_ancestor_hygiene() {
        assert_eq!(
            nominal_name(&name("testapi/types", "Pick_first"), NominalKind::Newtype),
            "KioModule_testapi.KioModule_types.Pick_first"
        );
        assert_eq!(
            nominal_name(&name("api/api", "Api"), NominalKind::Newtype),
            "KioModule_api.KioModule_D1_api.Api"
        );
        assert_eq!(
            nominal_name(&name("java/class", "Shapes"), NominalKind::Newtype),
            "KioModule_java.KioModule_class.KioType_Shapes"
        );
        assert_eq!(storage_name(&name("api", "Storage")), "KioStorage");
        assert_eq!(storage_name(&name("api", "Box")), "Storage");
        assert_ne!(
            nominal_name(&name("api/foo", "Bar"), NominalKind::Newtype),
            nominal_name(&name("api", "Foo_bar"), NominalKind::Newtype)
        );
    }

    #[test]
    fn classfile_identities_preserve_roles_and_source_words_under_case_folding() {
        let sources = [
            ("api/types", "Token"),
            ("api", "Types"),
            ("api/foo_bar", "Token"),
            ("api/foobar", "Token"),
            ("api", "Foo_bar"),
            ("api", "Foobar"),
            ("api", "Kiomodule_types"),
            ("api", "Kiotype_shapes"),
            ("api", "Kionominal_host"),
        ];
        let mut physical_keys = std::collections::BTreeSet::new();
        for (module, leaf) in sources {
            let path = nominal_path(&name(module, leaf), NominalKind::Newtype);
            let key = format!("Shapes${}.class", path.join("$")).to_ascii_lowercase();
            assert!(physical_keys.insert(key.clone()), "{module}.{leaf}: {key}");
            let mut facade_path = module
                .split('/')
                .map(|part| FacadeSelector::Module(part.to_owned()))
                .collect::<Vec<_>>();
            facade_path.push(FacadeSelector::Type(leaf.to_owned()));
            let key = format!("Package${}.class", facade_class_name(&facade_path, "NtH"))
                .to_ascii_lowercase();
            assert!(physical_keys.insert(key.clone()), "{module}.{leaf}: {key}");
        }
        for module in ["foo_bar", "foobar"] {
            let path = [FacadeSelector::Module(module.to_owned())];
            let key =
                format!("Package${}.class", facade_class_name(&path, "Ns")).to_ascii_lowercase();
            assert!(physical_keys.insert(key));
        }
        assert_eq!(
            nominal_name(&name("api", "Kiomodule_types"), NominalKind::Newtype),
            "KioModule_api.KioType_Kiomodule_utypes"
        );
    }

    #[test]
    fn facade_class_growth_uses_bounded_exact_role_identity() {
        let source = std::iter::repeat_n("a", 80).collect::<Vec<_>>().join("_");
        let path = [FacadeSelector::Module(source.clone())];
        let class = facade_class_name(&path, "Ns");
        assert_eq!(
            class,
            format!(
                "Ns_H{}",
                fnv1a_64_hex(&format!("Ns_M_{}", encode_source_identity(&source)))
            )
        );
        assert!(class.len() <= crate::backends::skin::READABLE_MINT_MAX_LEN);
        assert_ne!(class, facade_class_name(&path, "NtH"));
        assert_ne!(
            class,
            facade_class_name(&[FacadeSelector::Module(source.replace('_', ""))], "Ns")
        );
    }

    #[test]
    fn complete_nominal_classfile_names_are_bounded() {
        for depth in [1, 8, 32, 256] {
            for width in [1, 30, 300] {
                let module = vec!["a".repeat(width); depth].join("/");
                let source = name(&module, "Storage");
                let path = nominal_path(&source, NominalKind::Host);
                let basename = format!("Shapes${}${}.class", path.join("$"), storage_name(&source));
                assert!(basename.len() <= NOMINAL_CLASSFILE_BUDGET, "{basename}");
                if depth == 256 || width == 300 {
                    assert_eq!(path.len(), 1, "oversized paths have no namespace holders");
                }
            }
        }
    }

    #[test]
    fn whole_path_cutoff_reserves_the_longest_helper() {
        let width = NOMINAL_CLASSFILE_BUDGET
            - CLASSFILE_ENVELOPE
            - "KioModule_".len()
            - "Storage".len()
            - 1;
        let at_limit = name(&"a".repeat(width), "Storage");
        let readable = nominal_path(&at_limit, NominalKind::Host);
        assert_eq!(readable.len(), 2);
        assert_eq!(
            format!("Shapes${}$KioStorage.class", readable.join("$")).len(),
            NOMINAL_CLASSFILE_BUDGET
        );
        let beyond = name(&"a".repeat(width + 1), "Storage");
        assert_eq!(nominal_path(&beyond, NominalKind::Host).len(), 1);
        assert_eq!(storage_name(&beyond), "Storage");
        assert_ne!(
            nominal_path(&beyond, NominalKind::Host),
            nominal_path(&beyond, NominalKind::Newtype)
        );
    }
}
