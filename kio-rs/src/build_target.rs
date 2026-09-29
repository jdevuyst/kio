//! Build-target vocabulary shared by dispatch, key validation, and tooling.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTarget {
    Js,
    Ts,
    Go,
    Python,
    Java,
    Swift,
    Haskell,
    Rust,
    KioPrime,
}

impl BuildTarget {
    pub const ALL: &'static [Self] = &[
        Self::Js,
        Self::Ts,
        Self::Go,
        Self::Python,
        Self::Java,
        Self::Swift,
        Self::Haskell,
        Self::Rust,
        Self::KioPrime,
    ];

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|target| target.id() == id)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Js => "js",
            Self::Ts => "ts",
            Self::Go => "go",
            Self::Python => "python",
            Self::Java => "java",
            Self::Swift => "swift",
            Self::Haskell => "haskell",
            Self::Rust => "rust",
            Self::KioPrime => "kio-prime",
        }
    }

    pub fn keys(self) -> &'static [&'static str] {
        match self {
            Self::KioPrime => &["out"],
            Self::Rust => &["out", "namespace", "thread_safety"],
            Self::Js
            | Self::Ts
            | Self::Go
            | Self::Python
            | Self::Java
            | Self::Swift
            | Self::Haskell => &["out", "namespace"],
        }
    }

    #[cfg(feature = "cli")]
    pub(crate) fn key(self, name: &str) -> Option<&'static str> {
        self.keys().iter().copied().find(|key| *key == name)
    }
}

#[cfg(test)]
mod tests {
    use super::BuildTarget;

    #[test]
    fn catalog_preserves_existing_dispatch_ids_and_key_boundaries() {
        let expected = [
            ("js", &["out", "namespace"][..]),
            ("ts", &["out", "namespace"][..]),
            ("go", &["out", "namespace"][..]),
            ("python", &["out", "namespace"][..]),
            ("java", &["out", "namespace"][..]),
            ("swift", &["out", "namespace"][..]),
            ("haskell", &["out", "namespace"][..]),
            ("rust", &["out", "namespace", "thread_safety"][..]),
            ("kio-prime", &["out"][..]),
        ];
        assert_eq!(BuildTarget::ALL.len(), expected.len());
        for (target, (id, keys)) in BuildTarget::ALL.iter().copied().zip(expected) {
            assert_eq!(target.id(), id);
            assert_eq!(BuildTarget::from_id(id), Some(target));
            assert_eq!(target.keys(), keys);
            assert!(!target.keys().contains(&"crate_name"));
            #[cfg(feature = "cli")]
            {
                for key in keys {
                    assert_eq!(target.key(key), Some(*key));
                }
                assert_eq!(target.key("crate_name"), None);
                assert_eq!(target.key("unknown"), None);
            }
        }
        for unknown in ["", "JS", "prime", "unknown"] {
            assert_eq!(BuildTarget::from_id(unknown), None);
        }
    }
}
