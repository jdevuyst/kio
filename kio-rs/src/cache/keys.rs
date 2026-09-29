use std::borrow::Borrow;
use std::fmt;

use blake3::Hasher;

use crate::package_collection::PackageKey;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct PackageName(String);

impl PackageName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn from_package_key(key: &PackageKey) -> Self {
        Self::new(key.package_name.clone())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for PackageName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PipelineTag(&'static str);

impl PipelineTag {
    pub fn new(tag: &'static str) -> Self {
        Self(tag)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for PipelineTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclaredModulePath(String);

impl DeclaredModulePath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for DeclaredModulePath {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for DeclaredModulePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageModuleKey(String);

impl PackageModuleKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn from_declared(
        declared: &DeclaredModulePath,
        _package_name: Option<&PackageName>,
    ) -> Self {
        Self::new(declared.as_str())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl Borrow<str> for PackageModuleKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PackageModuleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SourceHash(String);

impl SourceHash {
    pub fn new(hash: impl Into<String>) -> Self {
        Self(hash.into())
    }

    pub fn from_parts(parts: &[&[u8]]) -> Self {
        Self(hash_framed_parts(parts))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SurfaceFingerprint(String);

impl SurfaceFingerprint {
    pub fn new(fingerprint: impl Into<String>) -> Self {
        Self(fingerprint.into())
    }

    pub fn from_parts(parts: &[&[u8]]) -> Self {
        Self(hash_framed_parts(parts))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SurfaceFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct CacheNamespace(String);

impl CacheNamespace {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self(namespace.into())
    }

    pub fn from_package_key(key: &PackageKey) -> Self {
        let mut h = Hasher::new();
        write_framed(&mut h, key.package_name.as_bytes());
        write_framed(&mut h, key.canonical_dir.to_string_lossy().as_bytes());
        Self::new(h.finalize().to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CacheNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheTarget(String);

impl CacheTarget {
    pub fn new(target: impl Into<String>) -> Self {
        Self(target.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn path_component(&self) -> String {
        sanitize_path_component(self.as_str())
    }
}

impl fmt::Display for CacheTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactTargetProfileFingerprint(String);

impl ArtifactTargetProfileFingerprint {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(hash_bytes(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactInputFingerprint(String);

impl ArtifactInputFingerprint {
    pub fn from_parts(parts: &[&[u8]]) -> Self {
        Self(hash_framed_parts(parts))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmitTargetProfileFingerprint(String);

impl EmitTargetProfileFingerprint {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(hash_bytes(bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmitInputFingerprint(String);

impl EmitInputFingerprint {
    pub fn from_parts(parts: &[&[u8]]) -> Self {
        Self(hash_framed_parts(parts))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EquivRenderMode(String);

impl EquivRenderMode {
    pub fn new(mode: impl Into<String>) -> Self {
        Self(mode.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EquivRenderMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedModuleDependency {
    SamePackageModule {
        module: PackageModuleKey,
        fingerprint: SurfaceFingerprint,
    },
    SamePackageElaboratorModule {
        module: PackageModuleKey,
        fingerprint: SurfaceFingerprint,
    },
}

impl TypedModuleDependency {
    pub fn same_package_module(module: PackageModuleKey, fingerprint: SurfaceFingerprint) -> Self {
        Self::SamePackageModule {
            module,
            fingerprint,
        }
    }

    pub fn same_package_elaborator_module(
        module: PackageModuleKey,
        fingerprint: SurfaceFingerprint,
    ) -> Self {
        Self::SamePackageElaboratorModule {
            module,
            fingerprint,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::SamePackageModule { module, .. } => {
                format!("same-package:{}", module.as_str())
            }
            Self::SamePackageElaboratorModule { module, .. } => {
                format!("same-package-elaborator:{}", module.as_str())
            }
        }
    }

    pub fn fingerprint(&self) -> SurfaceFingerprint {
        match self {
            Self::SamePackageModule { fingerprint, .. }
            | Self::SamePackageElaboratorModule { fingerprint, .. } => fingerprint.clone(),
        }
    }
}

fn write_framed(h: &mut Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn hash_framed_parts(parts: &[&[u8]]) -> String {
    let mut h = Hasher::new();
    for part in parts {
        write_framed(&mut h, part);
    }
    h.finalize().to_hex().to_string()
}

fn sanitize_path_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_' => {
                out.push(char::from(b));
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
