//! Backend-neutral semantic plans for typed host boundaries.
//!
//! The pure [`BoundaryFacadePlan`] layer plans one prepared Routed scheme:
//! every lexical binder is a single-segment path, every nominal is
//! module-qualified, transparent aliases have already been unfolded, and the
//! caller supplies the exact semantic-nominal snapshot used by the structural
//! key rule. Nominal paths are stopping points; their type arguments are
//! planned, but nominal declarations are never unfolded by the pure planner.
//!
//! The crate-private `prepare_*_boundary_callable` layer is deliberately
//! declaration- and package-aware. It resolves one exact exported function,
//! host function, or public newtype member in its declaring module; preserves
//! its raw grouped source heads for private layout; qualifies every nominal in
//! that declaring scope; and deep-unfolds transparent aliases into a separate
//! semantic scheme. It does not materialize a catalog. The package-boundary
//! transaction owns complete site enumeration, the one semantic-nominal
//! snapshot, and catalog construction.
//!
//! The plan is an arena rather than a recursive Rust type. A Forall node is one
//! scoped type stage. A Function node is one value stage whose slots are fixed
//! from the prepared scheme before substitution: a canonical ABI-nullary Unit
//! domain has no slots, while every other domain contributes its complete
//! right product spine. Unit introduced into an existing value slot by type
//! substitution therefore remains one slot. Substitution replaces binder uses
//! inside the existing arena and never rescans a substituted type to change a
//! parent function's slot count.
//!
//! Product and sum shells are generic. Their semantic identity contains only
//! structural kind and ordered semantic keys; concrete payloads and nested
//! payload topology remain on the facade use. Both canonical codec spellings,
//! [`FacadeShellId::encode_public`] and
//! [`FacadeShellId::encode_exact_fallback`], are reversible and derived only
//! from that identity. A backend chooses between them only from fixed target-
//! path constraints, never registry occupancy or declaration occurrence; its
//! skin validates the selected rendered paths together with the complete
//! public host namespace.
//!
//! The open-world argument has the same two levels. Pure planning reads only
//! its prepared scheme and fixed semantic-nominal snapshot. Authoritative
//! preparation resolves bare names and transparent alias bodies only in each
//! declaration's own lexical scope, then carries exact qualified identities;
//! it never scans unrelated modules by leaf name. Adding an unrelated
//! declaration therefore cannot change an existing node, key, shell identity,
//! or encoded name.
//!
//! [`PreparedBoundaryCallableSites::collect`] is the sole package-wide
//! materializer. It covers live declarations carried by [`Package`] and, when
//! supplied, removed env-side host functions from one [`ReplayedInterface`].
//! A retained root is resolved only through its own version-exact frozen type
//! closure. It never reads the live package, another root's closure, or the
//! replayed live inventory when qualifying names, unfolding aliases, or
//! choosing semantic nominal keys. Its raw declaration must agree with the
//! normalized env contract, and a root-local kind walk validates every frozen
//! application and alias substitution before facade planning. Live sites carry
//! their private execution layout. Retained sites carry retirement metadata
//! and a non-executable source-presentation layout only, so a runtime layout
//! for a declaration that no longer exists remains unrepresentable. Backend
//! realization, host-language rendering, and complete rendered-namespace
//! validation remain separate concerns.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::{self, Write as _};
use std::ops::Range;

use crate::ast::{
    FnTypeCapabilities, Import, ImportItem, ImportKind, Item, Kind, NewtypeHostSurface, Role,
    Routed, SigItem, Signature, SignatureGroupRef, Surface, Type,
};
use crate::pass::resolve::Package;
use crate::sig::{
    ContractKind, ContractSide, FrozenTypeClosure, FrozenTypeItem, QualifiedName, RemovedItem,
    ReplayedInterface,
};
use crate::span::Span;

/// Compatibility version of the backend-neutral named-shell codec.
///
/// This is spelling compatibility, not part of FacadeShellId's semantic
/// identity.
pub const PUBLIC_FACADE_CODEC_VERSION: u32 = 1;

const PUBLIC_READABLE_CODEC_PREFIX: &str = "KioFacade_V";
const PUBLIC_EXACT_CODEC_PREFIX: &str = "KioFacadeX_";
const PUBLIC_CODEC_MAGIC: &[u8; 3] = b"KFS";

/// Exact module-qualified identity of one nominal type declaration.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct QualifiedTypeName {
    module_segments: Vec<String>,
    name: String,
}

impl QualifiedTypeName {
    /// Construct a qualified name. At least one non-empty module segment and
    /// a non-empty leaf are required.
    pub fn new(module_segments: Vec<String>, name: impl Into<String>) -> Option<Self> {
        let name = name.into();
        if module_segments.is_empty()
            || module_segments.iter().any(String::is_empty)
            || name.is_empty()
        {
            return None;
        }
        Some(Self {
            module_segments,
            name,
        })
    }

    fn from_path_segments(segments: &[crate::ast::PathSegment]) -> Option<Self> {
        let (name, module) = segments.split_last()?;
        Self::new(
            module.iter().map(|segment| segment.name.clone()).collect(),
            name.name.clone(),
        )
    }

    pub fn module_segments(&self) -> &[String] {
        &self.module_segments
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The selected public key of one product slot or sum arm.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticKey {
    Bare {
        name: String,
    },
    Qualified {
        module_segments: Vec<String>,
        name: String,
    },
    Positional {
        index: u64,
    },
}

/// Structural kind of one generic facade shell.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FacadeKind {
    Product,
    Sum,
}

/// Pure public identity of one generic product or sum shell.
///
/// Payload types are deliberately absent. They are generic arguments on each
/// FacadeUse, so equal semantic interfaces share one shell without sharing or
/// erasing their concrete payloads.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FacadeShellId {
    kind: FacadeKind,
    ordered_keys: Vec<SemanticKey>,
}

impl FacadeShellId {
    pub fn new(kind: FacadeKind, ordered_keys: Vec<SemanticKey>) -> Self {
        Self { kind, ordered_keys }
    }

    pub fn kind(&self) -> FacadeKind {
        self.kind
    }

    pub fn ordered_keys(&self) -> &[SemanticKey] {
        &self.ordered_keys
    }

    /// Exact product shell carrying one value for every semantic position of
    /// this shell. Backends use this when a host API must group per-branch
    /// handlers (for example, a wide sum eliminator) without re-selecting or
    /// renumbering the sum's keys locally.
    pub(crate) fn product_companion(&self) -> Self {
        Self::new(FacadeKind::Product, self.ordered_keys.clone())
    }

    /// Encode the complete identity with readable, reversible role frames.
    ///
    /// The common anonymous binary shells keep their exact friendly names.
    /// Every other identity spells out its structural kind, arity, B/Q/P key
    /// roles, and escaped source components. The result uses only ASCII
    /// letters, digits, and underscores and is injective without consulting a
    /// registry.
    pub fn encode_public(&self) -> String {
        if self.is_anonymous_binary(FacadeKind::Product) {
            return "Product".to_owned();
        }
        if self.is_anonymous_binary(FacadeKind::Sum) {
            return "Sum".to_owned();
        }

        let mut encoded = String::new();
        encoded.push_str(PUBLIC_READABLE_CODEC_PREFIX);
        write!(&mut encoded, "{}_", PUBLIC_FACADE_CODEC_VERSION)
            .expect("writing to String cannot fail");
        encoded.push_str(match self.kind {
            FacadeKind::Product => "Product_",
            FacadeKind::Sum => "Sum_",
        });
        write!(&mut encoded, "K{}_", self.ordered_keys.len())
            .expect("writing to String cannot fail");
        for key in &self.ordered_keys {
            match key {
                SemanticKey::Bare { name } => {
                    write_component_frame(&mut encoded, 'B', name);
                }
                SemanticKey::Qualified {
                    module_segments,
                    name,
                } => {
                    write!(&mut encoded, "Q{}_", module_segments.len())
                        .expect("writing to String cannot fail");
                    for segment in module_segments {
                        write_component_frame(&mut encoded, 'M', segment);
                    }
                    write_component_frame(&mut encoded, 'N', name);
                }
                SemanticKey::Positional { index } => {
                    write!(&mut encoded, "P{index}_").expect("writing to String cannot fail");
                }
            }
        }
        encoded
    }

    /// Encode the same identity as an opaque exact frame.
    ///
    /// A host renderer may select this spelling when its target syntax or
    /// path-chunking rules cannot carry the readable form. It is not a hash
    /// and is never selected from namespace occupancy.
    pub fn encode_exact_fallback(&self) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(PUBLIC_CODEC_MAGIC);
        bytes.extend_from_slice(&PUBLIC_FACADE_CODEC_VERSION.to_be_bytes());
        bytes.push(match self.kind {
            FacadeKind::Product => 0,
            FacadeKind::Sum => 1,
        });
        put_u64(&mut bytes, usize_to_u64(self.ordered_keys.len()));
        for key in &self.ordered_keys {
            match key {
                SemanticKey::Bare { name } => {
                    bytes.push(0);
                    put_string(&mut bytes, name);
                }
                SemanticKey::Qualified {
                    module_segments,
                    name,
                } => {
                    bytes.push(1);
                    put_u64(&mut bytes, usize_to_u64(module_segments.len()));
                    for segment in module_segments {
                        put_string(&mut bytes, segment);
                    }
                    put_string(&mut bytes, name);
                }
                SemanticKey::Positional { index } => {
                    bytes.push(2);
                    put_u64(&mut bytes, *index);
                }
            }
        }

        let mut encoded = String::with_capacity(PUBLIC_EXACT_CODEC_PREFIX.len() + bytes.len() * 2);
        encoded.push_str(PUBLIC_EXACT_CODEC_PREFIX);
        for byte in bytes {
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        encoded
    }

    /// Decode an exact public identity. Non-canonical alternative spellings,
    /// malformed frames, truncation, and trailing data are rejected.
    pub fn decode_public(encoded: &str) -> Result<Self, PublicIdentityCodecError> {
        let special = match encoded {
            "Product" => Some(Self::new(
                FacadeKind::Product,
                vec![
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ],
            )),
            "Sum" => Some(Self::new(
                FacadeKind::Sum,
                vec![
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ],
            )),
            _ => None,
        };
        if let Some(identity) = special {
            return Ok(identity);
        }

        if encoded.starts_with(PUBLIC_READABLE_CODEC_PREFIX) {
            return Self::decode_readable(encoded);
        }
        if encoded.starts_with(PUBLIC_EXACT_CODEC_PREFIX) {
            return Self::decode_exact_fallback(encoded);
        }
        Err(PublicIdentityCodecError::new("missing facade codec prefix"))
    }

    fn decode_readable(encoded: &str) -> Result<Self, PublicIdentityCodecError> {
        let mut cursor = TextCursor::new(encoded);
        cursor.consume(PUBLIC_READABLE_CODEC_PREFIX)?;
        let codec_version = cursor.read_u32_frame()?;
        if codec_version != PUBLIC_FACADE_CODEC_VERSION {
            return Err(PublicIdentityCodecError::new(
                "unsupported facade codec version",
            ));
        }
        let kind = if cursor.consume_if("Product_") {
            FacadeKind::Product
        } else if cursor.consume_if("Sum_") {
            FacadeKind::Sum
        } else {
            return Err(PublicIdentityCodecError::new(
                "unknown readable facade kind",
            ));
        };
        cursor.consume("K")?;
        let key_count = cursor.read_len_frame()?;
        if key_count > cursor.remaining() {
            return Err(PublicIdentityCodecError::new(
                "facade key count exceeds the remaining frame",
            ));
        }
        let mut ordered_keys = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            let key = match cursor.read_byte()? {
                b'B' => SemanticKey::Bare {
                    name: cursor.read_component()?,
                },
                b'Q' => {
                    let segment_count = cursor.read_len_frame()?;
                    if segment_count > cursor.remaining() {
                        return Err(PublicIdentityCodecError::new(
                            "qualified segment count exceeds the remaining frame",
                        ));
                    }
                    let mut module_segments = Vec::with_capacity(segment_count);
                    for _ in 0..segment_count {
                        cursor.consume("M")?;
                        module_segments.push(cursor.read_component()?);
                    }
                    cursor.consume("N")?;
                    SemanticKey::Qualified {
                        module_segments,
                        name: cursor.read_component()?,
                    }
                }
                b'P' => SemanticKey::Positional {
                    index: cursor.read_u64_frame()?,
                },
                _ => {
                    return Err(PublicIdentityCodecError::new(
                        "unknown readable semantic key tag",
                    ));
                }
            };
            ordered_keys.push(key);
        }
        if cursor.remaining() != 0 {
            return Err(PublicIdentityCodecError::new(
                "trailing data after readable facade identity",
            ));
        }
        let identity = Self::new(kind, ordered_keys);
        if identity.encode_public() != encoded {
            return Err(PublicIdentityCodecError::new(
                "non-canonical readable facade identity spelling",
            ));
        }
        Ok(identity)
    }

    fn decode_exact_fallback(encoded: &str) -> Result<Self, PublicIdentityCodecError> {
        let hex = encoded
            .strip_prefix(PUBLIC_EXACT_CODEC_PREFIX)
            .ok_or_else(|| PublicIdentityCodecError::new("missing facade codec prefix"))?;
        let bytes = decode_lower_hex(hex)?;
        let mut cursor = ByteCursor::new(&bytes);
        if cursor.take(PUBLIC_CODEC_MAGIC.len())? != PUBLIC_CODEC_MAGIC {
            return Err(PublicIdentityCodecError::new("wrong facade codec magic"));
        }
        let codec_version = cursor.read_u32()?;
        if codec_version != PUBLIC_FACADE_CODEC_VERSION {
            return Err(PublicIdentityCodecError::new(
                "unsupported facade codec version",
            ));
        }
        let kind = match cursor.read_byte()? {
            0 => FacadeKind::Product,
            1 => FacadeKind::Sum,
            _ => return Err(PublicIdentityCodecError::new("unknown facade kind tag")),
        };
        let key_count = cursor.read_len()?;
        if key_count > cursor.remaining() {
            return Err(PublicIdentityCodecError::new(
                "facade key count exceeds the remaining frame",
            ));
        }
        let mut ordered_keys = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            let key = match cursor.read_byte()? {
                0 => SemanticKey::Bare {
                    name: cursor.read_string()?,
                },
                1 => {
                    let segment_count = cursor.read_len()?;
                    if segment_count > cursor.remaining() {
                        return Err(PublicIdentityCodecError::new(
                            "qualified segment count exceeds the remaining frame",
                        ));
                    }
                    let mut module_segments = Vec::with_capacity(segment_count);
                    for _ in 0..segment_count {
                        module_segments.push(cursor.read_string()?);
                    }
                    SemanticKey::Qualified {
                        module_segments,
                        name: cursor.read_string()?,
                    }
                }
                2 => SemanticKey::Positional {
                    index: cursor.read_u64()?,
                },
                _ => {
                    return Err(PublicIdentityCodecError::new("unknown semantic key tag"));
                }
            };
            ordered_keys.push(key);
        }
        if cursor.remaining() != 0 {
            return Err(PublicIdentityCodecError::new(
                "trailing bytes after facade identity",
            ));
        }

        let identity = Self::new(kind, ordered_keys);
        if identity.encode_exact_fallback() != encoded {
            return Err(PublicIdentityCodecError::new(
                "non-canonical exact facade identity spelling",
            ));
        }
        Ok(identity)
    }

    fn is_anonymous_binary(&self, kind: FacadeKind) -> bool {
        self.kind == kind
            && self.ordered_keys
                == [
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ]
    }
}

/// Failure to decode a public facade identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicIdentityCodecError {
    message: &'static str,
}

impl PublicIdentityCodecError {
    fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for PublicIdentityCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for PublicIdentityCodecError {}

fn write_component_frame(out: &mut String, role: char, value: &str) {
    let escaped = escape_component(value);
    write!(out, "{role}{}_{escaped}", escaped.len()).expect("writing to String cannot fail");
}

fn escape_component(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => escaped.push(char::from(byte)),
            b'_' => escaped.push_str("_u"),
            _ => {
                write!(&mut escaped, "_x{byte:02x}").expect("writing to String cannot fail");
            }
        }
    }
    escaped
}

fn unescape_component(encoded: &[u8]) -> Result<String, PublicIdentityCodecError> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut offset = 0usize;
    while offset < encoded.len() {
        match encoded[offset] {
            byte @ (b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9') => {
                bytes.push(byte);
                offset += 1;
            }
            b'_' if encoded.get(offset + 1) == Some(&b'u') => {
                bytes.push(b'_');
                offset += 2;
            }
            b'_' if encoded.get(offset + 1) == Some(&b'x') => {
                let pair = encoded.get(offset + 2..offset + 4).ok_or_else(|| {
                    PublicIdentityCodecError::new("truncated escaped facade component")
                })?;
                bytes.push((lower_hex_digit(pair[0])? << 4) | lower_hex_digit(pair[1])?);
                offset += 4;
            }
            _ => {
                return Err(PublicIdentityCodecError::new(
                    "invalid escaped facade component",
                ));
            }
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| PublicIdentityCodecError::new("facade component is not UTF-8"))
}

struct TextCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> TextCursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            offset: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn consume(&mut self, expected: &str) -> Result<(), PublicIdentityCodecError> {
        if self.consume_if(expected) {
            Ok(())
        } else {
            Err(PublicIdentityCodecError::new(
                "facade identity frame tag mismatch",
            ))
        }
    }

    fn consume_if(&mut self, expected: &str) -> bool {
        let end = self.offset.saturating_add(expected.len());
        if self.bytes.get(self.offset..end) == Some(expected.as_bytes()) {
            self.offset = end;
            true
        } else {
            false
        }
    }

    fn read_byte(&mut self) -> Result<u8, PublicIdentityCodecError> {
        let byte = *self
            .bytes
            .get(self.offset)
            .ok_or_else(|| PublicIdentityCodecError::new("truncated facade identity"))?;
        self.offset += 1;
        Ok(byte)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PublicIdentityCodecError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| PublicIdentityCodecError::new("facade frame length overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| PublicIdentityCodecError::new("truncated facade identity"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn read_u32_frame(&mut self) -> Result<u32, PublicIdentityCodecError> {
        u32::try_from(self.read_decimal_frame()?)
            .map_err(|_| PublicIdentityCodecError::new("facade codec version exceeds u32"))
    }

    fn read_u64_frame(&mut self) -> Result<u64, PublicIdentityCodecError> {
        self.read_decimal_frame()
    }

    fn read_len_frame(&mut self) -> Result<usize, PublicIdentityCodecError> {
        usize::try_from(self.read_decimal_frame()?)
            .map_err(|_| PublicIdentityCodecError::new("facade frame does not fit usize"))
    }

    fn read_decimal_frame(&mut self) -> Result<u64, PublicIdentityCodecError> {
        let rest = &self.bytes[self.offset..];
        let delimiter = rest
            .iter()
            .position(|byte| *byte == b'_')
            .ok_or_else(|| PublicIdentityCodecError::new("unterminated facade number frame"))?;
        let digits = &rest[..delimiter];
        if digits.is_empty()
            || digits.iter().any(|byte| !byte.is_ascii_digit())
            || (digits.len() > 1 && digits[0] == b'0')
        {
            return Err(PublicIdentityCodecError::new(
                "non-canonical facade number frame",
            ));
        }
        let mut value = 0u64;
        for digit in digits {
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(*digit - b'0')))
                .ok_or_else(|| PublicIdentityCodecError::new("facade number frame overflow"))?;
        }
        self.offset += delimiter + 1;
        Ok(value)
    }

    fn read_component(&mut self) -> Result<String, PublicIdentityCodecError> {
        let len = self.read_len_frame()?;
        unescape_component(self.take(len)?)
    }
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    put_u64(out, usize_to_u64(value.len()));
    out.extend_from_slice(value.as_bytes());
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).expect("Rust usize values fit the facade codec's u64 frame")
}

fn decode_lower_hex(hex: &str) -> Result<Vec<u8>, PublicIdentityCodecError> {
    if !hex.len().is_multiple_of(2) {
        return Err(PublicIdentityCodecError::new(
            "facade codec hex has odd length",
        ));
    }
    let mut decoded = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let high = lower_hex_digit(pair[0])?;
        let low = lower_hex_digit(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn lower_hex_digit(byte: u8) -> Result<u8, PublicIdentityCodecError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(PublicIdentityCodecError::new(
            "facade codec contains non-lowercase-hex data",
        )),
    }
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PublicIdentityCodecError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or_else(|| PublicIdentityCodecError::new("facade frame length overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| PublicIdentityCodecError::new("truncated facade identity"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn read_byte(&mut self) -> Result<u8, PublicIdentityCodecError> {
        Ok(self.take(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32, PublicIdentityCodecError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .expect("cursor returned the requested frame width");
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_u64(&mut self) -> Result<u64, PublicIdentityCodecError> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .expect("cursor returned the requested frame width");
        Ok(u64::from_be_bytes(bytes))
    }

    fn read_len(&mut self) -> Result<usize, PublicIdentityCodecError> {
        usize::try_from(self.read_u64()?)
            .map_err(|_| PublicIdentityCodecError::new("facade frame does not fit usize"))
    }

    fn read_string(&mut self) -> Result<String, PublicIdentityCodecError> {
        let len = self.read_len()?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| PublicIdentityCodecError::new("facade string is not UTF-8"))
    }
}

/// Structural validation error for the explicit prepared-scheme boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreparedSchemeError {
    EmptyBinder,
    EmptyPath,
    EmptyPathSegment,
    UnqualifiedNominal(String),
}

impl fmt::Display for PreparedSchemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBinder => f.write_str("prepared boundary scheme has an empty binder"),
            Self::EmptyPath => f.write_str("prepared boundary scheme has an empty type path"),
            Self::EmptyPathSegment => {
                f.write_str("prepared boundary scheme has an empty type-path segment")
            }
            Self::UnqualifiedNominal(name) => write!(
                f,
                "prepared boundary scheme leaves nominal type {name:?} unqualified"
            ),
        }
    }
}

impl std::error::Error for PreparedSchemeError {}

/// Borrowed, structurally validated view of a prepared Routed scheme.
///
/// Validation proves binder/path well-formedness and nominal qualification.
/// It cannot determine whether a qualified path denotes a transparent alias;
/// callers outside the declaration-aware preparation layer must supply a
/// scheme whose aliases were already unfolded in their declaring scopes.
#[derive(Clone, Copy, Debug)]
pub struct PreparedBoundaryScheme<'a> {
    scheme: &'a Type<Routed>,
    semantic_nominals: &'a BTreeSet<QualifiedTypeName>,
}

impl<'a> PreparedBoundaryScheme<'a> {
    pub fn try_new(
        scheme: &'a Type<Routed>,
        semantic_nominals: &'a BTreeSet<QualifiedTypeName>,
    ) -> Result<Self, PreparedSchemeError> {
        validate_prepared_scheme(scheme)?;
        Ok(Self {
            scheme,
            semantic_nominals,
        })
    }
}

fn validate_prepared_scheme(scheme: &Type<Routed>) -> Result<(), PreparedSchemeError> {
    enum Task<'a> {
        Enter(&'a Type<Routed>),
        ExitBinder,
    }

    let mut tasks = vec![Task::Enter(scheme)];
    let mut scope = Vec::<String>::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::ExitBinder => {
                scope.pop();
            }
            Task::Enter(ty) => match ty {
                Type::Path { segments, args, .. } => {
                    if segments.is_empty() {
                        return Err(PreparedSchemeError::EmptyPath);
                    }
                    if segments.iter().any(|segment| segment.name.is_empty()) {
                        return Err(PreparedSchemeError::EmptyPathSegment);
                    }
                    if let [name] = segments.as_slice()
                        && !scope.iter().rev().any(|binder| binder == &name.name)
                    {
                        return Err(PreparedSchemeError::UnqualifiedNominal(name.name.clone()));
                    }
                    tasks.extend(args.iter().rev().map(Task::Enter));
                }
                Type::Unit { .. } | Type::Bottom { .. } => {}
                Type::Function { param, ret, .. } => {
                    tasks.push(Task::Enter(ret));
                    tasks.push(Task::Enter(param));
                }
                Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                    tasks.push(Task::Enter(right));
                    tasks.push(Task::Enter(left));
                }
                Type::Forall { param, body, .. } => {
                    if param.name.is_empty() {
                        return Err(PreparedSchemeError::EmptyBinder);
                    }
                    scope.push(param.name.clone());
                    tasks.push(Task::ExitBinder);
                    tasks.push(Task::Enter(body));
                }
                Type::LabelSugar { ext, .. } => match *ext {},
                Type::Infer { ext, .. } => match *ext {},
                Type::Goal { ext, .. } => match *ext {},
            },
        }
    }
    Ok(())
}

/// Stable index of a facade-use arena node.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FacadeUseId(usize);

impl FacadeUseId {
    pub fn index(self) -> usize {
        self.0
    }
}

/// Stable scoped identity of one universal binder in a plan.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FacadeBinderId(usize);

impl FacadeBinderId {
    pub fn index(self) -> usize {
        self.0
    }
}

/// Metadata of one scoped universal binder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FacadeBinder {
    pub name: String,
    pub kind: Kind,
    pub span: Span,
}

/// One host-visible type use or callable stage in the boundary plan.
///
/// Child node IDs always precede their parent in the owning arena. The graph
/// is therefore clonable and substitutable with a forward iterative pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FacadeUse {
    Unit {
        span: Span,
    },
    Bottom {
        span: Span,
    },
    Bound {
        binder: FacadeBinderId,
        span: Span,
    },
    Nominal {
        name: QualifiedTypeName,
        span: Span,
    },
    Apply {
        constructor: FacadeUseId,
        args: Vec<FacadeUseId>,
        span: Span,
    },
    Product {
        shell: FacadeShellId,
        args: Vec<FacadeUseId>,
        span: Span,
    },
    Sum {
        shell: FacadeShellId,
        args: Vec<FacadeUseId>,
        span: Span,
    },
    Function {
        slots: Vec<FacadeUseId>,
        shell: Option<FacadeShellId>,
        result: FacadeUseId,
        caps: FnTypeCapabilities,
        span: Span,
    },
    Forall {
        binder: FacadeBinderId,
        result: FacadeUseId,
        span: Span,
    },
}

/// Complete pure plan for one prepared boundary scheme.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundaryFacadePlan {
    binders: Vec<FacadeBinder>,
    uses: Vec<FacadeUse>,
    root: FacadeUseId,
}

impl BoundaryFacadePlan {
    pub fn from_prepared_scheme(prepared: PreparedBoundaryScheme<'_>) -> Self {
        PlanBuilder::new(prepared.semantic_nominals, 0, false)
            .build(prepared.scheme)
            .0
    }

    fn from_prepared_scheme_with_execution(
        prepared: PreparedBoundaryScheme<'_>,
        declaration_binder_count: usize,
    ) -> (Self, BoundaryFacadeExecutionPlan) {
        let (facade, execution) =
            PlanBuilder::new(prepared.semantic_nominals, declaration_binder_count, true)
                .build(prepared.scheme);
        (
            facade,
            execution.expect("execution-enabled facade planning returns one parallel arena"),
        )
    }

    pub fn root(&self) -> FacadeUseId {
        self.root
    }

    pub fn use_at(&self, id: FacadeUseId) -> &FacadeUse {
        &self.uses[id.0]
    }

    pub fn binder(&self, id: FacadeBinderId) -> &FacadeBinder {
        &self.binders[id.0]
    }

    /// Exact product identity for one function value's semantic slots.
    /// Nullary and unary functions need no grouping shell.
    pub(crate) fn function_shell(&self, id: FacadeUseId) -> Option<&FacadeShellId> {
        match self.use_at(id) {
            FacadeUse::Function { shell, .. } => shell.as_ref(),
            _ => None,
        }
    }

    pub fn uses(&self) -> &[FacadeUse] {
        &self.uses
    }

    pub(crate) fn uses_with_ids(
        &self,
    ) -> impl DoubleEndedIterator<Item = (FacadeUseId, &FacadeUse)> + ExactSizeIterator {
        self.uses
            .iter()
            .enumerate()
            .map(|(index, use_)| (FacadeUseId(index), use_))
    }

    pub fn binders(&self) -> &[FacadeBinder] {
        &self.binders
    }

    /// Consume exactly one leading universal stage and replace its bound uses
    /// with another already-planned type use.
    ///
    /// The parent arena is cloned in topological order. Existing Function,
    /// Product, and Sum nodes retain their exact topology. In particular, a
    /// function argument replacing one opaque slot becomes that slot's nested
    /// use; it never becomes another value stage in the parent call.
    pub fn apply_leading_type_arg(&self, argument: &BoundaryFacadePlan) -> Option<Self> {
        let FacadeUse::Forall {
            binder: consumed,
            result: _,
            ..
        } = self.use_at(self.root)
        else {
            return None;
        };

        let mut out = Self {
            binders: Vec::new(),
            uses: Vec::new(),
            root: FacadeUseId(0),
        };
        let (_, argument_nodes) = append_plan(argument, &mut out);
        let argument_root = argument_nodes[argument.root.0];

        let mut binder_map = vec![None; self.binders.len()];
        for (index, binder) in self.binders.iter().enumerate() {
            let old = FacadeBinderId(index);
            if old == *consumed {
                continue;
            }
            let new = FacadeBinderId(out.binders.len());
            out.binders.push(binder.clone());
            binder_map[index] = Some(new);
        }

        let mut node_map = Vec::with_capacity(self.uses.len());
        for node in &self.uses {
            let mapped = match node {
                FacadeUse::Bound { binder, .. } if binder == consumed => argument_root,
                FacadeUse::Forall { binder, result, .. } if binder == consumed => {
                    node_map[result.0]
                }
                _ => {
                    let cloned = remap_use(node, &node_map, &binder_map);
                    let id = FacadeUseId(out.uses.len());
                    out.uses.push(cloned);
                    id
                }
            };
            node_map.push(mapped);
        }
        out.root = node_map[self.root.0];
        Some(out)
    }
}

fn append_plan(
    source: &BoundaryFacadePlan,
    destination: &mut BoundaryFacadePlan,
) -> (Vec<FacadeBinderId>, Vec<FacadeUseId>) {
    let binder_map = source
        .binders
        .iter()
        .map(|binder| {
            let id = FacadeBinderId(destination.binders.len());
            destination.binders.push(binder.clone());
            id
        })
        .collect::<Vec<_>>();
    let optional_binders = binder_map.iter().copied().map(Some).collect::<Vec<_>>();
    let mut node_map = Vec::with_capacity(source.uses.len());
    for node in &source.uses {
        let cloned = remap_use(node, &node_map, &optional_binders);
        let id = FacadeUseId(destination.uses.len());
        destination.uses.push(cloned);
        node_map.push(id);
    }
    (binder_map, node_map)
}

fn remap_use(
    source: &FacadeUse,
    nodes: &[FacadeUseId],
    binders: &[Option<FacadeBinderId>],
) -> FacadeUse {
    let node = |id: &FacadeUseId| nodes[id.0];
    let binder = |id: &FacadeBinderId| {
        binders[id.0].unwrap_or_else(|| {
            unreachable!("a consumed binder use is replaced before ordinary remapping")
        })
    };
    match source {
        FacadeUse::Unit { span } => FacadeUse::Unit { span: *span },
        FacadeUse::Bottom { span } => FacadeUse::Bottom { span: *span },
        FacadeUse::Bound {
            binder: source,
            span,
        } => FacadeUse::Bound {
            binder: binder(source),
            span: *span,
        },
        FacadeUse::Nominal { name, span } => FacadeUse::Nominal {
            name: name.clone(),
            span: *span,
        },
        FacadeUse::Apply {
            constructor,
            args,
            span,
        } => FacadeUse::Apply {
            constructor: node(constructor),
            args: args.iter().map(node).collect(),
            span: *span,
        },
        FacadeUse::Product { shell, args, span } => FacadeUse::Product {
            shell: shell.clone(),
            args: args.iter().map(node).collect(),
            span: *span,
        },
        FacadeUse::Sum { shell, args, span } => FacadeUse::Sum {
            shell: shell.clone(),
            args: args.iter().map(node).collect(),
            span: *span,
        },
        FacadeUse::Function {
            slots,
            shell,
            result,
            caps,
            span,
        } => FacadeUse::Function {
            slots: slots.iter().map(node).collect(),
            shell: shell.clone(),
            result: node(result),
            caps: caps.clone(),
            span: *span,
        },
        FacadeUse::Forall {
            binder: source,
            result,
            span,
        } => FacadeUse::Forall {
            binder: binder(source),
            result: node(result),
            span: *span,
        },
    }
}

struct PlanBuilder<'a> {
    semantic_nominals: &'a BTreeSet<QualifiedTypeName>,
    binders: Vec<FacadeBinder>,
    uses: Vec<FacadeUse>,
    execution_uses: Vec<BoundaryFacadeExecutionUse>,
    scope: Vec<(String, FacadeBinderId)>,
    declaration_binders_remaining: usize,
    record_execution: bool,
}

enum BuildTask<'a> {
    Enter(&'a Type<Routed>),
    FinishApply {
        constructor: FacadeUseId,
        arg_count: usize,
        span: Span,
    },
    FinishStructural {
        shell: FacadeShellId,
        arg_count: usize,
        span: Span,
    },
    FinishFunction {
        param: &'a Type<Routed>,
        slot_count: usize,
        abi_arity: usize,
        caps: FnTypeCapabilities,
        span: Span,
    },
    FinishForall {
        binder: FacadeBinderId,
        execution: BoundaryFacadeExecutionUse,
        span: Span,
    },
}

impl<'a> PlanBuilder<'a> {
    fn new(
        semantic_nominals: &'a BTreeSet<QualifiedTypeName>,
        declaration_binder_count: usize,
        record_execution: bool,
    ) -> Self {
        Self {
            semantic_nominals,
            binders: Vec::new(),
            uses: Vec::new(),
            execution_uses: Vec::new(),
            scope: Vec::new(),
            declaration_binders_remaining: declaration_binder_count,
            record_execution,
        }
    }

    fn build(
        mut self,
        scheme: &'a Type<Routed>,
    ) -> (BoundaryFacadePlan, Option<BoundaryFacadeExecutionPlan>) {
        let mut tasks = vec![BuildTask::Enter(scheme)];
        let mut completed = Vec::<FacadeUseId>::new();
        while let Some(task) = tasks.pop() {
            match task {
                BuildTask::Enter(ty) => self.enter(ty, &mut tasks, &mut completed),
                BuildTask::FinishApply {
                    constructor,
                    arg_count,
                    span,
                } => {
                    let args = take_completed(&mut completed, arg_count);
                    let id = self.push(
                        FacadeUse::Apply {
                            constructor,
                            args,
                            span,
                        },
                        BoundaryFacadeExecutionUse::NoAction,
                    );
                    completed.push(id);
                }
                BuildTask::FinishStructural {
                    shell,
                    arg_count,
                    span,
                } => {
                    let args = take_completed(&mut completed, arg_count);
                    let use_ = match shell.kind() {
                        FacadeKind::Product => FacadeUse::Product { shell, args, span },
                        FacadeKind::Sum => FacadeUse::Sum { shell, args, span },
                    };
                    let id = self.push(use_, BoundaryFacadeExecutionUse::NoAction);
                    completed.push(id);
                }
                BuildTask::FinishFunction {
                    param,
                    slot_count,
                    abi_arity,
                    caps,
                    span,
                } => {
                    let mut children = take_completed(&mut completed, slot_count + 1);
                    let result = children
                        .pop()
                        .expect("a function finish always receives its result");
                    let execution = if self.record_execution {
                        BoundaryFacadeExecutionUse::Function(
                            CallableValueStageLayout::from_facade_function(
                                param,
                                abi_arity,
                                &children,
                                &self.uses,
                                self.semantic_nominals,
                            ),
                        )
                    } else {
                        BoundaryFacadeExecutionUse::NoAction
                    };
                    let shell = (children.len() > 1).then(|| {
                        FacadeShellId::new(
                            FacadeKind::Product,
                            select_semantic_keys_from_facade_slots(
                                &children,
                                &self.uses,
                                self.semantic_nominals,
                            ),
                        )
                    });
                    let id = self.push(
                        FacadeUse::Function {
                            slots: children,
                            shell,
                            result,
                            caps,
                            span,
                        },
                        execution,
                    );
                    completed.push(id);
                }
                BuildTask::FinishForall {
                    binder,
                    execution,
                    span,
                } => {
                    let result = completed
                        .pop()
                        .expect("a forall finish always receives its body");
                    let (_, popped) = self
                        .scope
                        .pop()
                        .expect("a forall finish always leaves one binder scope");
                    debug_assert_eq!(popped, binder);
                    let id = self.push(
                        FacadeUse::Forall {
                            binder,
                            result,
                            span,
                        },
                        execution,
                    );
                    completed.push(id);
                }
            }
        }
        assert_eq!(
            completed.len(),
            1,
            "one prepared scheme must produce one facade root"
        );
        assert_eq!(
            self.declaration_binders_remaining, 0,
            "every declared payload binder has a leading synthetic forall"
        );
        let facade = BoundaryFacadePlan {
            binders: self.binders,
            uses: self.uses,
            root: completed[0],
        };
        let execution = self
            .record_execution
            .then_some(BoundaryFacadeExecutionPlan {
                uses: self.execution_uses,
            });
        if let Some(execution) = &execution {
            execution
                .validate_alignment(&facade)
                .unwrap_or_else(|reason| unreachable!("fresh facade execution plan: {reason}"));
        }
        (facade, execution)
    }

    fn enter(
        &mut self,
        ty: &'a Type<Routed>,
        tasks: &mut Vec<BuildTask<'a>>,
        completed: &mut Vec<FacadeUseId>,
    ) {
        match ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                let head = if let [name] = segments.as_slice() {
                    let binder = self
                        .scope
                        .iter()
                        .rev()
                        .find_map(|(candidate, id)| (candidate == &name.name).then_some(*id))
                        .unwrap_or_else(|| {
                            unreachable!("prepared single-segment paths are lexical binders")
                        });
                    self.push(
                        FacadeUse::Bound {
                            binder,
                            span: meta.span,
                        },
                        BoundaryFacadeExecutionUse::NoAction,
                    )
                } else {
                    let name = QualifiedTypeName::from_path_segments(segments)
                        .unwrap_or_else(|| unreachable!("prepared nominal paths are qualified"));
                    self.push(
                        FacadeUse::Nominal {
                            name,
                            span: meta.span,
                        },
                        BoundaryFacadeExecutionUse::NoAction,
                    )
                };
                if args.is_empty() {
                    completed.push(head);
                } else {
                    tasks.push(BuildTask::FinishApply {
                        constructor: head,
                        arg_count: args.len(),
                        span: meta.span,
                    });
                    tasks.extend(args.iter().rev().map(BuildTask::Enter));
                }
            }
            Type::Unit { meta } => {
                let id = self.push(
                    FacadeUse::Unit { span: meta.span },
                    BoundaryFacadeExecutionUse::NoAction,
                );
                completed.push(id);
            }
            Type::Bottom { meta } => {
                let id = self.push(
                    FacadeUse::Bottom { span: meta.span },
                    BoundaryFacadeExecutionUse::NoAction,
                );
                completed.push(id);
            }
            Type::Product { meta, .. } => {
                let slots = Type::right_spine_product(ty);
                let shell = FacadeShellId::new(
                    FacadeKind::Product,
                    select_semantic_keys(&slots, self.semantic_nominals),
                );
                tasks.push(BuildTask::FinishStructural {
                    shell,
                    arg_count: slots.len(),
                    span: meta.span,
                });
                tasks.extend(slots.into_iter().rev().map(BuildTask::Enter));
            }
            Type::Sum { meta, .. } => {
                let slots = Type::right_spine_sum(ty);
                let shell = FacadeShellId::new(
                    FacadeKind::Sum,
                    select_semantic_keys(&slots, self.semantic_nominals),
                );
                tasks.push(BuildTask::FinishStructural {
                    shell,
                    arg_count: slots.len(),
                    span: meta.span,
                });
                tasks.extend(slots.into_iter().rev().map(BuildTask::Enter));
            }
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => {
                let slots = if *abi_arity == 0 && matches!(param.as_ref(), Type::Unit { .. }) {
                    Vec::new()
                } else {
                    Type::right_spine_product(param)
                };
                tasks.push(BuildTask::FinishFunction {
                    param,
                    slot_count: slots.len(),
                    abi_arity: *abi_arity,
                    caps: caps.clone(),
                    span: meta.span,
                });
                tasks.push(BuildTask::Enter(ret));
                tasks.extend(slots.into_iter().rev().map(BuildTask::Enter));
            }
            Type::Forall {
                param, body, meta, ..
            } => {
                let binder = FacadeBinderId(self.binders.len());
                self.binders.push(FacadeBinder {
                    name: param.name.clone(),
                    kind: param.effective_kind(),
                    span: param.span,
                });
                self.scope.push((param.name.clone(), binder));
                let execution = if !self.record_execution {
                    BoundaryFacadeExecutionUse::NoAction
                } else if self.declaration_binders_remaining == 0 {
                    BoundaryFacadeExecutionUse::InvokeForall
                } else {
                    self.declaration_binders_remaining -= 1;
                    BoundaryFacadeExecutionUse::DeclarationBinder
                };
                tasks.push(BuildTask::FinishForall {
                    binder,
                    execution,
                    span: meta.span,
                });
                tasks.push(BuildTask::Enter(body));
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn push(&mut self, use_: FacadeUse, execution: BoundaryFacadeExecutionUse) -> FacadeUseId {
        let id = FacadeUseId(self.uses.len());
        self.uses.push(use_);
        self.execution_uses.push(execution);
        id
    }
}

fn take_completed(completed: &mut Vec<FacadeUseId>, count: usize) -> Vec<FacadeUseId> {
    let start = completed
        .len()
        .checked_sub(count)
        .expect("facade finish cannot consume more children than were planned");
    completed.split_off(start)
}

fn select_semantic_keys(
    slots: &[&Type<Routed>],
    semantic_nominals: &BTreeSet<QualifiedTypeName>,
) -> Vec<SemanticKey> {
    let mut claimed = BTreeSet::new();
    let mut selected = Vec::with_capacity(slots.len());
    for (index, slot) in slots.iter().enumerate() {
        let nominal = match slot {
            Type::Path { segments, .. } => QualifiedTypeName::from_path_segments(segments)
                .filter(|name| semantic_nominals.contains(name)),
            _ => None,
        };
        let candidates = nominal
            .map(|name| {
                vec![
                    SemanticKey::Bare {
                        name: name.name.clone(),
                    },
                    SemanticKey::Qualified {
                        module_segments: name.module_segments.clone(),
                        name: name.name,
                    },
                ]
            })
            .unwrap_or_default();
        let key = candidates
            .into_iter()
            .find(|candidate| !claimed.contains(candidate))
            .unwrap_or_else(|| SemanticKey::Positional {
                index: usize_to_u64(index),
            });
        claimed.insert(key.clone());
        selected.push(key);
    }
    selected
}

fn select_semantic_keys_from_facade_slots(
    slots: &[FacadeUseId],
    facade_uses: &[FacadeUse],
    semantic_nominals: &BTreeSet<QualifiedTypeName>,
) -> Vec<SemanticKey> {
    let mut claimed = BTreeSet::new();
    let mut selected = Vec::with_capacity(slots.len());
    for (index, slot) in slots.iter().enumerate() {
        let mut head = *slot;
        while let FacadeUse::Apply { constructor, .. } = &facade_uses[head.0] {
            head = *constructor;
        }
        let nominal = match &facade_uses[head.0] {
            FacadeUse::Nominal { name, .. } if semantic_nominals.contains(name) => Some(name),
            _ => None,
        };
        let candidates = nominal
            .map(|name| {
                vec![
                    SemanticKey::Bare {
                        name: name.name.clone(),
                    },
                    SemanticKey::Qualified {
                        module_segments: name.module_segments.clone(),
                        name: name.name.clone(),
                    },
                ]
            })
            .unwrap_or_default();
        let key = candidates
            .into_iter()
            .find(|candidate| !claimed.contains(candidate))
            .unwrap_or_else(|| SemanticKey::Positional {
                index: usize_to_u64(index),
            });
        claimed.insert(key.clone());
        selected.push(key);
    }
    selected
}

/// Kind of one declaration-owned stage consumed before a callable boundary
/// returns its declared result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundaryCallableHeadStageKind {
    Type,
    Value,
}

/// A facade plan paired with the declaration-head cut derived from the same
/// exact [`Signature`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundaryCallablePlan {
    facade: BoundaryFacadePlan,
    head_stage_kinds: Vec<BoundaryCallableHeadStageKind>,
    head_value_shell: Option<FacadeShellId>,
}

impl BoundaryCallablePlan {
    /// Plan one declaration from its exact canonical signature and return.
    /// The signature's value-group boundaries, not an independently chosen
    /// prefix of the assembled scheme, determine the declaration-owned cut.
    #[cfg(test)]
    fn from_prepared_signature(
        signature: &Signature<Routed>,
        ret: &Type<Routed>,
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
    ) -> Result<Self, PreparedSchemeError> {
        let scheme = signature.signature_ty(ret.clone(), ret.span());
        Self::from_authoritative_scheme(
            &scheme,
            signature_head_stage_kinds(signature),
            semantic_nominals,
        )
    }

    #[cfg(test)]
    fn from_authoritative_scheme(
        scheme: &Type<Routed>,
        head_stage_kinds: Vec<BoundaryCallableHeadStageKind>,
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
    ) -> Result<Self, PreparedSchemeError> {
        let prepared = PreparedBoundaryScheme::try_new(scheme, semantic_nominals)?;
        Ok(Self::with_facade(
            BoundaryFacadePlan::from_prepared_scheme(prepared),
            head_stage_kinds,
            semantic_nominals,
        ))
    }

    fn from_authoritative_scheme_with_execution(
        scheme: &Type<Routed>,
        head_stage_kinds: Vec<BoundaryCallableHeadStageKind>,
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
    ) -> Result<(Self, BoundaryFacadeExecutionPlan), PreparedSchemeError> {
        let prepared = PreparedBoundaryScheme::try_new(scheme, semantic_nominals)?;
        let (facade, execution) =
            BoundaryFacadePlan::from_prepared_scheme_with_execution(prepared, 0);
        Ok((
            Self::with_facade(facade, head_stage_kinds, semantic_nominals),
            execution,
        ))
    }

    fn with_facade(
        facade: BoundaryFacadePlan,
        head_stage_kinds: Vec<BoundaryCallableHeadStageKind>,
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
    ) -> Self {
        let mut plan = Self {
            facade,
            head_stage_kinds,
            head_value_shell: None,
        };
        let slots = plan
            .entry()
            .head_stages
            .into_iter()
            .flat_map(|stage| match stage {
                BoundaryCallableHeadStage::Type { .. } => [].as_slice(),
                BoundaryCallableHeadStage::Value { slots } => slots,
            })
            .copied()
            .collect::<Vec<_>>();
        plan.head_value_shell = (slots.len() > 1).then(|| {
            FacadeShellId::new(
                FacadeKind::Product,
                select_semantic_keys_from_facade_slots(
                    &slots,
                    plan.facade.uses(),
                    semantic_nominals,
                ),
            )
        });
        plan
    }

    pub fn facade(&self) -> &BoundaryFacadePlan {
        &self.facade
    }

    pub fn head_stage_kinds(&self) -> &[BoundaryCallableHeadStageKind] {
        &self.head_stage_kinds
    }

    /// Exact product identity for all declaration-head value stages after the
    /// public facade compacts them into one host call.
    pub(crate) fn head_value_shell(&self) -> Option<&FacadeShellId> {
        self.head_value_shell.as_ref()
    }

    pub fn entry(&self) -> BoundaryCallableEntry<'_> {
        let mut current = self.facade.root;
        let mut stages = Vec::with_capacity(self.head_stage_kinds.len());
        for expected in &self.head_stage_kinds {
            match (expected, self.facade.use_at(current)) {
                (BoundaryCallableHeadStageKind::Type, FacadeUse::Forall { binder, result, .. }) => {
                    stages.push(BoundaryCallableHeadStage::Type {
                        id: *binder,
                        binder: self.facade.binder(*binder),
                    });
                    current = *result;
                }
                (
                    BoundaryCallableHeadStageKind::Value,
                    FacadeUse::Function { slots, result, .. },
                ) => {
                    stages.push(BoundaryCallableHeadStage::Value { slots });
                    current = *result;
                }
                _ => unreachable!("a signature-derived cut matches its own callable scheme"),
            }
        }
        BoundaryCallableEntry {
            head_stages: stages,
            returned: current,
        }
    }
}

/// Borrowed declaration-head projection of one callable plan.
#[derive(Debug)]
pub struct BoundaryCallableEntry<'a> {
    pub head_stages: Vec<BoundaryCallableHeadStage<'a>>,
    pub returned: FacadeUseId,
}

/// One projected declaration-owned stage.
#[derive(Clone, Copy, Debug)]
pub enum BoundaryCallableHeadStage<'a> {
    Type {
        id: FacadeBinderId,
        binder: &'a FacadeBinder,
    },
    Value {
        slots: &'a [FacadeUseId],
    },
}

/// Semantic owner of one package-boundary facade plan.
///
/// The role stays separate from the source spelling because Kio's module,
/// type, and value namespaces can admit the same leaf without denoting the
/// same boundary site.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BoundaryFacadeSiteOwner {
    HostFunction { name: String },
    ExportedFunction { name: String },
    NewtypeConstructor { newtype: String, member: String },
    NewtypeProjector { newtype: String, member: String },
}

impl BoundaryFacadeSiteOwner {
    fn is_valid(&self) -> bool {
        match self {
            Self::HostFunction { name } | Self::ExportedFunction { name } => !name.is_empty(),
            Self::NewtypeConstructor { newtype, member }
            | Self::NewtypeProjector { newtype, member } => {
                !newtype.is_empty() && !member.is_empty()
            }
        }
    }
}

/// Stable package-local identity of one exact boundary use.
///
/// Source module components and selector role remain structured. Source
/// spans, declaration encounter order, target namespace, rendered names, and
/// payload representation are deliberately absent.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BoundaryFacadeSiteId {
    module_segments: Vec<String>,
    owner: BoundaryFacadeSiteOwner,
}

impl BoundaryFacadeSiteId {
    pub fn new(module_segments: Vec<String>, owner: BoundaryFacadeSiteOwner) -> Option<Self> {
        if module_segments.is_empty()
            || module_segments.iter().any(String::is_empty)
            || !owner.is_valid()
        {
            return None;
        }
        Some(Self {
            module_segments,
            owner,
        })
    }

    pub fn module_segments(&self) -> &[String] {
        &self.module_segments
    }

    pub fn owner(&self) -> &BoundaryFacadeSiteOwner {
        &self.owner
    }
}

/// One declaration type parameter retained by a site's nominal projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryNominalTypeParam {
    name: String,
    kind: Kind,
}

impl BoundaryNominalTypeParam {
    fn from_source(param: &crate::ast::TypeParam) -> Self {
        Self {
            name: param.name.clone(),
            kind: param.effective_kind(),
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn kind(&self) -> &Kind {
        &self.kind
    }
}

/// Exact host-binding category of one nominal `host type` declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryHostTypeBinding {
    Role(Role),
    Roleless,
}

/// Whether one exact host-type binding belongs to the live package contract or
/// survives only as a source-stability slot from sealed signature history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryHostBindingOrigin {
    Live,
    Retained { removed_at_version: u32 },
}

/// Live-dominant provenance of one generated facade support declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryFacadeSupportOrigin {
    Live,
    Retained { removed_at_version: u32 },
}

impl BoundaryFacadeSupportOrigin {
    fn join(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::Live, _) | (_, Self::Live) => Self::Live,
            (
                Self::Retained {
                    removed_at_version: left,
                },
                Self::Retained {
                    removed_at_version: right,
                },
            ) => Self::Retained {
                removed_at_version: left.min(right),
            },
        }
    }

    pub(crate) fn removed_at_version(self) -> Option<u32> {
        match self {
            Self::Live => None,
            Self::Retained { removed_at_version } => Some(removed_at_version),
        }
    }
}

/// One declaration-keyed host-type choice in the package-root binding plan.
///
/// The identity is semantic and exact. Two declarations carrying the same
/// role remain two independent choices, while a retained declaration keeps its
/// old slot without becoming a live nominal dependency or callable capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryHostBinding {
    name: QualifiedTypeName,
    type_params: Vec<BoundaryNominalTypeParam>,
    binding: BoundaryHostTypeBinding,
    /// Whether the source declaration carried the redundant `{ owned }`
    /// annotation. Live declarations retain the bit for source-faithful
    /// documentation; signature replay deliberately drops it because the
    /// annotation selects no Rust representation.
    owned: bool,
    origin: BoundaryHostBindingOrigin,
}

impl BoundaryHostBinding {
    pub(crate) fn name(&self) -> &QualifiedTypeName {
        &self.name
    }

    pub(crate) fn type_params(&self) -> &[BoundaryNominalTypeParam] {
        &self.type_params
    }

    pub(crate) fn binding(&self) -> BoundaryHostTypeBinding {
        self.binding
    }

    pub(crate) fn owned(&self) -> bool {
        self.owned
    }

    pub(crate) fn origin(&self) -> BoundaryHostBindingOrigin {
        self.origin
    }
}

/// Public member surface that selects a newtype's host-boundary treatment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryNewtypeSurface {
    Unexposed,
    Opaque,
    Constructor {
        member: String,
    },
    Projector {
        member: String,
    },
    Both {
        constructor: String,
        projector: String,
    },
}

impl BoundaryNewtypeSurface {
    fn from_host_surface<P: crate::ast::Phase>(
        surface: Option<&NewtypeHostSurface<'_, P>>,
    ) -> Self {
        match surface {
            None => Self::Unexposed,
            Some(NewtypeHostSurface::Opaque) => Self::Opaque,
            Some(NewtypeHostSurface::Constructor { constructor, .. }) => Self::Constructor {
                member: constructor.name.clone(),
            },
            Some(NewtypeHostSurface::Projector { projector, .. }) => Self::Projector {
                member: projector.name.clone(),
            },
            Some(NewtypeHostSurface::Both {
                constructor,
                projector,
                ..
            }) => Self::Both {
                constructor: constructor.name.clone(),
                projector: projector.name.clone(),
            },
        }
    }

    pub(crate) fn uses_nominal_carrier(&self) -> bool {
        !matches!(self, Self::Both { .. })
    }
}

/// One live, bridged public newtype and its exact callable surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryPublicNewtypeInventoryEntry {
    name: QualifiedTypeName,
    type_params: Vec<BoundaryNominalTypeParam>,
    existential_params: Vec<BoundaryNominalTypeParam>,
    surface: BoundaryNewtypeSurface,
}

impl BoundaryPublicNewtypeInventoryEntry {
    pub(crate) fn name(&self) -> &QualifiedTypeName {
        &self.name
    }

    pub(crate) fn type_params(&self) -> &[BoundaryNominalTypeParam] {
        &self.type_params
    }

    pub(crate) fn existential_params(&self) -> &[BoundaryNominalTypeParam] {
        &self.existential_params
    }

    pub(crate) fn surface(&self) -> &BoundaryNewtypeSurface {
        &self.surface
    }
}

/// Authoritative declaration-payload planning owned by one nominal snapshot.
///
/// These shells are dependencies available to a backend that selects the
/// payload's transparent realization. They are not automatically public shape
/// declarations: a backend must first apply its exact transparent/recursive
/// reachability policy, then use these already-planned identities without
/// reparsing or replanning the declaration payload. The facade records only
/// semantic public shape, never runtime ABI or conversion-layout metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryNewtypePayloadPlan {
    facade: BoundaryFacadePlan,
    shells: BTreeSet<FacadeShellId>,
    declaration_binders: Vec<FacadeBinderId>,
    payload_root: FacadeUseId,
}

impl BoundaryNewtypePayloadPlan {
    pub(crate) fn facade(&self) -> &BoundaryFacadePlan {
        &self.facade
    }

    pub(crate) fn shell_dependencies(
        &self,
    ) -> impl DoubleEndedIterator<Item = &FacadeShellId> + ExactSizeIterator {
        self.shells.iter()
    }

    pub(crate) fn declaration_binders(&self) -> &[FacadeBinderId] {
        &self.declaration_binders
    }

    pub(crate) fn payload_root(&self) -> FacadeUseId {
        self.payload_root
    }
}

/// Same-snapshot declaration facts for one exact nominal dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryNominalDeclaration {
    HostType {
        type_params: Vec<BoundaryNominalTypeParam>,
        binding: BoundaryHostTypeBinding,
    },
    Newtype {
        type_params: Vec<BoundaryNominalTypeParam>,
        existential_params: Vec<BoundaryNominalTypeParam>,
        transparent_payload: Option<BoundaryNewtypePayloadPlan>,
        surface: BoundaryNewtypeSurface,
    },
}

fn semantically_equal_nominal_declarations(
    left: &BoundaryNominalDeclaration,
    right: &BoundaryNominalDeclaration,
) -> bool {
    if !semantically_equal_nominal_headers(left, right) {
        return false;
    }
    match (left, right) {
        (
            BoundaryNominalDeclaration::HostType {
                type_params: left_params,
                binding: left_binding,
            },
            BoundaryNominalDeclaration::HostType {
                type_params: right_params,
                binding: right_binding,
            },
        ) => {
            let _ = (left_params, right_params, left_binding, right_binding);
            true
        }
        (
            BoundaryNominalDeclaration::Newtype {
                type_params: left_params,
                existential_params: left_existentials,
                transparent_payload: left_payload,
                surface: left_surface,
            },
            BoundaryNominalDeclaration::Newtype {
                type_params: right_params,
                existential_params: right_existentials,
                transparent_payload: right_payload,
                surface: right_surface,
            },
        ) => {
            let _ = (
                left_params,
                right_params,
                left_existentials,
                right_existentials,
                left_surface,
                right_surface,
            );
            match (left_payload, right_payload) {
                (None, None) => true,
                (Some(left), Some(right)) => {
                    semantically_equal_facade_plans_alpha(left.facade(), right.facade())
                        && left.shells == right.shells
                        && left.declaration_binders.len() == right.declaration_binders.len()
                        && left.payload_root.index() == right.payload_root.index()
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn semantically_equal_nominal_headers(
    left: &BoundaryNominalDeclaration,
    right: &BoundaryNominalDeclaration,
) -> bool {
    let params_equal = |left: &[BoundaryNominalTypeParam], right: &[BoundaryNominalTypeParam]| {
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(left, right)| left.kind == right.kind)
    };
    match (left, right) {
        (
            BoundaryNominalDeclaration::HostType {
                type_params: left_params,
                binding: left_binding,
            },
            BoundaryNominalDeclaration::HostType {
                type_params: right_params,
                binding: right_binding,
            },
        ) => params_equal(left_params, right_params) && left_binding == right_binding,
        (
            BoundaryNominalDeclaration::Newtype {
                type_params: left_params,
                existential_params: left_existentials,
                surface: left_surface,
                ..
            },
            BoundaryNominalDeclaration::Newtype {
                type_params: right_params,
                existential_params: right_existentials,
                surface: right_surface,
                ..
            },
        ) => {
            params_equal(left_params, right_params)
                && params_equal(left_existentials, right_existentials)
                && left_surface == right_surface
        }
        _ => false,
    }
}

fn semantically_equal_facade_plans_alpha(
    left: &BoundaryFacadePlan,
    right: &BoundaryFacadePlan,
) -> bool {
    left.root.index() == right.root.index()
        && left.binders.len() == right.binders.len()
        && left
            .binders
            .iter()
            .zip(&right.binders)
            .all(|(left, right)| left.kind == right.kind)
        && left.uses.len() == right.uses.len()
        && left.uses.iter().zip(&right.uses).all(|(left, right)| {
            let ids_equal = |left: FacadeUseId, right: FacadeUseId| left.index() == right.index();
            let id_lists_equal = |left: &[FacadeUseId], right: &[FacadeUseId]| {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| left.index() == right.index())
            };
            match (left, right) {
                (FacadeUse::Unit { .. }, FacadeUse::Unit { .. })
                | (FacadeUse::Bottom { .. }, FacadeUse::Bottom { .. }) => true,
                (FacadeUse::Bound { binder: left, .. }, FacadeUse::Bound { binder: right, .. }) => {
                    left.index() == right.index()
                }
                (FacadeUse::Nominal { name: left, .. }, FacadeUse::Nominal { name: right, .. }) => {
                    left == right
                }
                (
                    FacadeUse::Apply {
                        constructor: left_constructor,
                        args: left_args,
                        ..
                    },
                    FacadeUse::Apply {
                        constructor: right_constructor,
                        args: right_args,
                        ..
                    },
                ) => {
                    ids_equal(*left_constructor, *right_constructor)
                        && id_lists_equal(left_args, right_args)
                }
                (
                    FacadeUse::Product {
                        shell: left_shell,
                        args: left_args,
                        ..
                    }
                    | FacadeUse::Sum {
                        shell: left_shell,
                        args: left_args,
                        ..
                    },
                    FacadeUse::Product {
                        shell: right_shell,
                        args: right_args,
                        ..
                    }
                    | FacadeUse::Sum {
                        shell: right_shell,
                        args: right_args,
                        ..
                    },
                ) => left_shell == right_shell && id_lists_equal(left_args, right_args),
                (
                    FacadeUse::Function {
                        slots: left_slots,
                        shell: left_shell,
                        result: left_result,
                        caps: left_caps,
                        ..
                    },
                    FacadeUse::Function {
                        slots: right_slots,
                        shell: right_shell,
                        result: right_result,
                        caps: right_caps,
                        ..
                    },
                ) => {
                    id_lists_equal(left_slots, right_slots)
                        && left_shell == right_shell
                        && ids_equal(*left_result, *right_result)
                        && left_caps == right_caps
                }
                (
                    FacadeUse::Forall {
                        binder: left_binder,
                        result: left_result,
                        ..
                    },
                    FacadeUse::Forall {
                        binder: right_binder,
                        result: right_result,
                        ..
                    },
                ) => {
                    left_binder.index() == right_binder.index()
                        && ids_equal(*left_result, *right_result)
                }
                _ => false,
            }
        })
}

/// Root-local declarations needed to interpret every nominal in one plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryNominalDependencies {
    declarations: BTreeMap<QualifiedTypeName, BoundaryNominalDeclaration>,
}

impl BoundaryNominalDependencies {
    pub(crate) fn declaration(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<&BoundaryNominalDeclaration> {
        self.declarations.get(name)
    }

    pub(crate) fn declarations(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&QualifiedTypeName, &BoundaryNominalDeclaration)>
    + ExactSizeIterator {
        self.declarations.iter()
    }
}

/// One declaration identity occurred more than once in catalog collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryFacadeSiteConflict {
    #[cfg(test)]
    site: BoundaryFacadeSiteId,
}

impl BoundaryFacadeSiteConflict {
    #[cfg(test)]
    pub(crate) fn site(&self) -> &BoundaryFacadeSiteId {
        &self.site
    }
}

impl fmt::Display for BoundaryFacadeSiteConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("one boundary facade site was collected more than once")
    }
}

impl std::error::Error for BoundaryFacadeSiteConflict {}

/// Failure to collect one authoritative package declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryFacadeCollectionError {
    MissingDeclaration {
        module_path: String,
        selector: String,
    },
    InconsistentRetainedRoot {
        module_path: String,
        name: String,
        reason: &'static str,
    },
    UnresolvedRetainedType {
        module_path: String,
        path: Vec<String>,
    },
    RetainedAliasCycle {
        path: Vec<QualifiedName>,
    },
    RetainedAliasArity {
        name: QualifiedName,
        expected: usize,
        actual: usize,
    },
    InvalidRetainedKind {
        module_path: String,
        message: String,
    },
    InvalidRetainedSubstitution {
        module_path: String,
        binder: String,
    },
    MissingNominalDependency {
        site: Box<BoundaryFacadeSiteId>,
        name: QualifiedTypeName,
    },
    UnexpectedAliasDependency {
        site: Box<BoundaryFacadeSiteId>,
        name: QualifiedTypeName,
    },
    InvalidNominalDependency {
        site: BoundaryFacadeSiteId,
        name: String,
        reason: &'static str,
    },
    InvalidPublicNewtypeInventory {
        name: QualifiedTypeName,
        reason: &'static str,
    },
    InvalidHostBindingInventory {
        name: QualifiedTypeName,
        reason: &'static str,
    },
    Duplicate(BoundaryFacadeSiteConflict),
    InvalidScheme(PreparedSchemeError),
}

impl fmt::Display for BoundaryFacadeCollectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDeclaration {
                module_path,
                selector,
            } => write!(
                f,
                "the requested package-boundary {selector} does not exist in module \
                 {module_path:?}"
            ),
            Self::InconsistentRetainedRoot {
                module_path,
                name,
                reason,
            } => write!(
                f,
                "the retained host-function root {module_path}.{name} is inconsistent: {reason}"
            ),
            Self::UnresolvedRetainedType { module_path, path } => write!(
                f,
                "the retained type {} referenced from module {module_path:?} is absent from the \
                 root's frozen type closure",
                path.join(".")
            ),
            Self::RetainedAliasCycle { path } => write!(
                f,
                "the retained type-alias cycle has no nominal boundary: {}",
                path.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::RetainedAliasArity {
                name,
                expected,
                actual,
            } => write!(
                f,
                "the retained type alias {name} expects {expected} argument(s), but the frozen \
                 root supplies {actual}"
            ),
            Self::InvalidRetainedKind {
                module_path,
                message,
            } => write!(
                f,
                "the retained type rooted in module {module_path:?} is kind-invalid: {message}"
            ),
            Self::InvalidRetainedSubstitution {
                module_path,
                binder,
            } => write!(
                f,
                "the retained alias binder {binder:?} in module {module_path:?} cannot receive \
                 the frozen type arguments at that application site"
            ),
            Self::MissingNominalDependency { site, name } => write!(
                f,
                "boundary site {:?} references nominal type {}.{}, but that exact declaration is \
                 absent from the site's source snapshot",
                site.owner(),
                name.module_segments().join("."),
                name.name(),
            ),
            Self::UnexpectedAliasDependency { site, name } => write!(
                f,
                "boundary site {:?} retains transparent alias {}.{} after canonicalization",
                site.owner(),
                name.module_segments().join("."),
                name.name(),
            ),
            Self::InvalidNominalDependency { site, name, reason } => write!(
                f,
                "boundary site {:?} has an invalid nominal dependency {name:?}: {reason}",
                site.owner(),
            ),
            Self::InvalidPublicNewtypeInventory { name, reason } => write!(
                f,
                "live public newtype {}.{} has an inconsistent boundary inventory: {reason}",
                name.module_segments().join("."),
                name.name(),
            ),
            Self::InvalidHostBindingInventory { name, reason } => write!(
                f,
                "exact host binding {}.{} has an inconsistent live/retained inventory: {reason}",
                name.module_segments().join("."),
                name.name(),
            ),
            Self::Duplicate(error) => error.fmt(f),
            Self::InvalidScheme(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for BoundaryFacadeCollectionError {}

impl From<BoundaryFacadeSiteConflict> for BoundaryFacadeCollectionError {
    fn from(error: BoundaryFacadeSiteConflict) -> Self {
        Self::Duplicate(error)
    }
}

impl From<PreparedSchemeError> for BoundaryFacadeCollectionError {
    fn from(error: PreparedSchemeError) -> Self {
        Self::InvalidScheme(error)
    }
}

/// Exact owner-derived callable projection shared by semantic collection and
/// private backend execution-layout planning.
///
/// The catalog consumes only the semantic plan derived from this value. The
/// raw scheme remains a separate crate-internal source of truth so a backend
/// skin does not re-qualify or re-synthesize the declaration independently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AuthoritativeBoundaryHeadStage {
    Type { param: crate::ast::TypeParam },
    Value { source_params: Vec<Type<Routed>> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthoritativeBoundaryCallable {
    site: BoundaryFacadeSiteId,
    #[cfg(test)]
    exact_scheme: Type<Routed>,
    semantic_scheme: Type<Routed>,
    source_head_stages: Vec<AuthoritativeBoundaryHeadStage>,
    head_stage_kinds: Vec<BoundaryCallableHeadStageKind>,
}

impl AuthoritativeBoundaryCallable {
    fn from_signature(
        site: BoundaryFacadeSiteId,
        signature: &Signature<Routed>,
        ret: &Type<Routed>,
        package: &Package<Routed>,
    ) -> Self {
        let exact_scheme = signature.signature_ty(ret.clone(), ret.span());
        let source_head_stages = signature_source_head_stages(signature);
        Self {
            site,
            semantic_scheme: erase_live_comptime_type_names(
                canonicalize_authoritative_scheme(&exact_scheme, package, None),
                Vec::new(),
            ),
            #[cfg(test)]
            exact_scheme,
            head_stage_kinds: source_head_stage_kinds(&source_head_stages),
            source_head_stages,
        }
    }

    fn from_scheme(
        site: BoundaryFacadeSiteId,
        exact_scheme: Type<Routed>,
        source_head_stages: Vec<AuthoritativeBoundaryHeadStage>,
        package: &Package<Routed>,
    ) -> Self {
        Self {
            site,
            semantic_scheme: erase_live_comptime_type_names(
                canonicalize_authoritative_scheme(&exact_scheme, package, None),
                Vec::new(),
            ),
            #[cfg(test)]
            exact_scheme,
            head_stage_kinds: source_head_stage_kinds(&source_head_stages),
            source_head_stages,
        }
    }

    fn semantic_plan(
        &self,
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
    ) -> Result<(BoundaryCallablePlan, BoundaryFacadeExecutionPlan), PreparedSchemeError> {
        BoundaryCallablePlan::from_authoritative_scheme_with_execution(
            &self.semantic_scheme,
            self.head_stage_kinds.clone(),
            semantic_nominals,
        )
    }

    #[cfg(test)]
    pub(crate) fn site(&self) -> &BoundaryFacadeSiteId {
        &self.site
    }

    #[cfg(test)]
    pub(crate) fn exact_scheme(&self) -> &Type<Routed> {
        &self.exact_scheme
    }

    #[cfg(test)]
    pub(crate) fn semantic_scheme(&self) -> &Type<Routed> {
        &self.semantic_scheme
    }

    #[cfg(test)]
    pub(crate) fn head_stage_kinds(&self) -> &[BoundaryCallableHeadStageKind] {
        &self.head_stage_kinds
    }

    #[cfg(test)]
    pub(crate) fn source_head_stages(&self) -> &[AuthoritativeBoundaryHeadStage] {
        &self.source_head_stages
    }
}

/// Runtime action for one declaration-owned type stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallableTypeStageAction {
    InvokeNullary,
}

/// Adapter from one frozen source parameter to its public facade slots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallableSourceParamAdapter {
    UnitValue,
    Identity,
    RightNest,
}

/// Frozen public-slot range and body adapter for one source parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallableSourceParamLayout {
    facade_slots: Range<usize>,
    adapter: CallableSourceParamAdapter,
    product_shell: Option<FacadeShellId>,
}

impl CallableSourceParamLayout {
    pub(crate) fn facade_slots(&self) -> Range<usize> {
        self.facade_slots.clone()
    }

    pub(crate) fn adapter(&self) -> CallableSourceParamAdapter {
        self.adapter
    }

    /// The canonical product identity reconstructed by a `RightNest` adapter.
    /// `UnitValue` and `Identity` parameters do not own a product shell.
    pub(crate) fn product_shell(&self) -> Option<&FacadeShellId> {
        self.product_shell.as_ref()
    }
}

/// Private execution layout of one declaration-owned value stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallableValueStageLayout {
    source_param_count: usize,
    body_abi_arity: usize,
    facade_slot_count: usize,
    source_params: Vec<CallableSourceParamLayout>,
    facade_shell: Option<FacadeShellId>,
}

impl CallableValueStageLayout {
    fn from_canonical_source_params(
        source_params: &[&Type<Routed>],
        facade_slots: &[FacadeUseId],
        facade_uses: &[FacadeUse],
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
    ) -> Self {
        let source_param_count = source_params.len();
        let facade_slot_count = facade_slots.len();
        let mut next_slot = 0usize;
        let mut layouts = Vec::with_capacity(source_param_count);
        for (index, param) in source_params.iter().copied().enumerate() {
            let is_final = index + 1 == source_param_count;
            let (slot_count, adapter) = if !is_final {
                (1, CallableSourceParamAdapter::Identity)
            } else if source_param_count == 1
                && facade_slot_count == 0
                && matches!(param, Type::Unit { .. })
            {
                (0, CallableSourceParamAdapter::UnitValue)
            } else if matches!(param, Type::Product { .. }) {
                let slots = Type::right_spine_product(param);
                (slots.len(), CallableSourceParamAdapter::RightNest)
            } else {
                (1, CallableSourceParamAdapter::Identity)
            };
            let end = next_slot
                .checked_add(slot_count)
                .expect("a callable facade slot range fits in usize");
            let product_shell = (adapter == CallableSourceParamAdapter::RightNest).then(|| {
                FacadeShellId::new(
                    FacadeKind::Product,
                    select_semantic_keys_from_facade_slots(
                        &facade_slots[next_slot..end],
                        facade_uses,
                        semantic_nominals,
                    ),
                )
            });
            layouts.push(CallableSourceParamLayout {
                facade_slots: next_slot..end,
                adapter,
                product_shell,
            });
            next_slot = end;
        }
        assert_eq!(
            next_slot, facade_slot_count,
            "canonical source adapters cover the paired semantic facade slots exactly"
        );
        let facade_shell = (facade_slot_count > 1).then(|| {
            FacadeShellId::new(
                FacadeKind::Product,
                select_semantic_keys_from_facade_slots(
                    facade_slots,
                    facade_uses,
                    semantic_nominals,
                ),
            )
        });

        Self {
            source_param_count,
            body_abi_arity: source_param_count,
            facade_slot_count,
            source_params: layouts,
            facade_shell,
        }
    }

    /// Freeze one nested function use from its original Routed ABI partition
    /// and the semantic slots selected during that same facade walk.
    fn from_facade_function(
        param: &Type<Routed>,
        body_abi_arity: usize,
        facade_slots: &[FacadeUseId],
        facade_uses: &[FacadeUse],
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
    ) -> Self {
        let source_params = Type::right_spine_take(param, body_abi_arity);
        Self::from_canonical_source_params(
            &source_params,
            facade_slots,
            facade_uses,
            semantic_nominals,
        )
    }

    pub(crate) fn source_param_count(&self) -> usize {
        self.source_param_count
    }

    pub(crate) fn body_abi_arity(&self) -> usize {
        self.body_abi_arity
    }

    pub(crate) fn facade_slot_count(&self) -> usize {
        self.facade_slot_count
    }

    pub(crate) fn source_params(&self) -> &[CallableSourceParamLayout] {
        &self.source_params
    }

    /// Exact product identity for the complete public value stage.
    ///
    /// Most backends expose its slots directly. A host with a hard callable
    /// arity ceiling can instead accept this one prepared shell and use the
    /// same source-parameter layout to reconstruct the private call.
    pub(crate) fn facade_shell(&self) -> Option<&FacadeShellId> {
        self.facade_shell.as_ref()
    }
}

/// Exact source-presentation topology shared by live and retained callables.
///
/// This view freezes declaration-head source partitions and nested
/// function/forall presentation. It intentionally carries no transparent
/// payload conversion, direct-projector compaction, or body-call authority;
/// retained compatibility shims may render it but cannot execute it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallablePresentationLayout {
    head_stages: Vec<CallableExecutionStage>,
    root_uses: BoundaryFacadeExecutionPlan,
    transparent_payloads: BTreeMap<QualifiedTypeName, BoundaryFacadeExecutionPlan>,
}

impl CallablePresentationLayout {
    fn from_authoritative(
        callable: &AuthoritativeBoundaryCallable,
        plan: &BoundaryCallablePlan,
        nominals: &BoundaryNominalDependencies,
        root_uses: BoundaryFacadeExecutionPlan,
        transparent_payloads: BTreeMap<QualifiedTypeName, BoundaryFacadeExecutionPlan>,
    ) -> Self {
        let semantic_nominals = nominals
            .declarations()
            .filter(|(_, declaration)| {
                matches!(declaration, BoundaryNominalDeclaration::Newtype { .. })
            })
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        let mut canonical = &callable.semantic_scheme;
        let entry = plan.entry();
        assert_eq!(callable.source_head_stages.len(), entry.head_stages.len());
        let mut head_stages = Vec::with_capacity(callable.source_head_stages.len());
        for (source, semantic) in callable.source_head_stages.iter().zip(entry.head_stages) {
            match (source, semantic) {
                (
                    AuthoritativeBoundaryHeadStage::Type {
                        param: source_param,
                    },
                    BoundaryCallableHeadStage::Type { binder, .. },
                ) => {
                    let Type::Forall {
                        param: canonical_param,
                        body,
                        ..
                    } = canonical
                    else {
                        unreachable!("a canonical presentation type head retains its forall")
                    };
                    assert_eq!(source_param.name, canonical_param.name);
                    assert_eq!(source_param.name, binder.name);
                    assert_eq!(source_param.effective_kind(), binder.kind);
                    head_stages.push(CallableExecutionStage::Type {
                        action: CallableTypeStageAction::InvokeNullary,
                    });
                    canonical = body;
                }
                (
                    AuthoritativeBoundaryHeadStage::Value { source_params },
                    BoundaryCallableHeadStage::Value { slots },
                ) => {
                    let Type::Function { param, ret, .. } = canonical else {
                        unreachable!("a canonical presentation value head retains its function")
                    };
                    let canonical_source_params =
                        Type::right_spine_take(param, source_params.len());
                    assert_eq!(canonical_source_params.len(), source_params.len());
                    head_stages.push(CallableExecutionStage::Value(
                        CallableValueStageLayout::from_canonical_source_params(
                            &canonical_source_params,
                            slots,
                            plan.facade().uses(),
                            &semantic_nominals,
                        ),
                    ));
                    canonical = ret;
                }
                _ => unreachable!("a callable presentation agrees on every head stage"),
            }
        }
        let presentation = Self {
            head_stages,
            root_uses,
            transparent_payloads,
        };
        presentation.assert_alignment(plan, nominals);
        presentation
    }

    pub(crate) fn head_stages(&self) -> &[CallableExecutionStage] {
        &self.head_stages
    }

    pub(crate) fn root_uses(&self) -> &BoundaryFacadeExecutionPlan {
        &self.root_uses
    }

    pub(crate) fn transparent_payload(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<&BoundaryFacadeExecutionPlan> {
        self.transparent_payloads.get(name)
    }

    fn assert_alignment(
        &self,
        plan: &BoundaryCallablePlan,
        nominals: &BoundaryNominalDependencies,
    ) {
        assert_stage_alignment_with(plan, &self.head_stages);
        self.root_uses
            .validate_alignment(plan.facade())
            .unwrap_or_else(|reason| unreachable!("callable presentation alignment: {reason}"));
        let expected = nominals
            .declarations()
            .filter_map(|(name, declaration)| match declaration {
                BoundaryNominalDeclaration::Newtype {
                    transparent_payload: Some(_),
                    ..
                } => Some(name.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            self.transparent_payloads
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            expected,
            "transparent presentation keys equal semantic payload keys"
        );
        for (name, presentation) in &self.transparent_payloads {
            let Some(BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            }) = nominals.declaration(name)
            else {
                unreachable!("a transparent presentation key denotes a payload")
            };
            presentation
                .validate_alignment(payload.facade())
                .unwrap_or_else(|reason| {
                    unreachable!("transparent payload presentation alignment: {reason}")
                });
        }
    }
}

/// Live-only execution action paired 1:1 with one semantic facade use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BoundaryFacadeExecutionUse {
    NoAction,
    Function(CallableValueStageLayout),
    InvokeForall,
    DeclarationBinder,
}

/// Live-only execution topology parallel to a semantic facade arena.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundaryFacadeExecutionPlan {
    uses: Vec<BoundaryFacadeExecutionUse>,
}

impl BoundaryFacadeExecutionPlan {
    pub(crate) fn use_at(&self, id: FacadeUseId) -> &BoundaryFacadeExecutionUse {
        &self.uses[id.0]
    }

    #[cfg(test)]
    pub(crate) fn uses(&self) -> &[BoundaryFacadeExecutionUse] {
        &self.uses
    }

    fn validate_alignment(&self, facade: &BoundaryFacadePlan) -> Result<(), &'static str> {
        if self.uses.len() != facade.uses().len() {
            return Err("execution and semantic facade arenas have different lengths");
        }
        for (semantic, execution) in facade.uses().iter().zip(&self.uses) {
            match (semantic, execution) {
                (
                    FacadeUse::Function { slots, .. },
                    BoundaryFacadeExecutionUse::Function(layout),
                ) => {
                    validate_function_execution_layout(layout, slots.len())?;
                }
                (
                    FacadeUse::Forall { .. },
                    BoundaryFacadeExecutionUse::InvokeForall
                    | BoundaryFacadeExecutionUse::DeclarationBinder,
                ) => {}
                (
                    FacadeUse::Unit { .. }
                    | FacadeUse::Bottom { .. }
                    | FacadeUse::Bound { .. }
                    | FacadeUse::Nominal { .. }
                    | FacadeUse::Apply { .. }
                    | FacadeUse::Product { .. }
                    | FacadeUse::Sum { .. },
                    BoundaryFacadeExecutionUse::NoAction,
                ) => {}
                _ => return Err("one execution entry disagrees with its semantic facade use"),
            }
        }
        Ok(())
    }
}

fn validate_function_execution_layout(
    layout: &CallableValueStageLayout,
    facade_slot_count: usize,
) -> Result<(), &'static str> {
    if layout.facade_slot_count != facade_slot_count
        || layout.source_param_count != layout.body_abi_arity
        || layout.source_param_count != layout.source_params.len()
    {
        return Err("a function execution layout has inconsistent counts");
    }
    if layout.body_abi_arity == 0 {
        return if facade_slot_count == 0
            && layout.source_params.is_empty()
            && layout.facade_shell.is_none()
        {
            Ok(())
        } else {
            Err("an ABI-nullary function execution layout retains parameters")
        };
    }
    if facade_slot_count == 0 {
        let [source] = layout.source_params.as_slice() else {
            return Err("a non-nullary zero-slot function must retain one Unit source parameter");
        };
        return if layout.source_param_count == 1
            && layout.body_abi_arity == 1
            && source.facade_slots == (0..0)
            && source.adapter == CallableSourceParamAdapter::UnitValue
            && source.product_shell.is_none()
            && layout.facade_shell.is_none()
        {
            Ok(())
        } else {
            Err("a non-nullary zero-slot function must adapt exactly one Unit source parameter")
        };
    }
    if facade_slot_count < layout.body_abi_arity {
        return Err("a function facade has fewer slots than ABI parameters");
    }
    let facade_shell_matches = if facade_slot_count > 1 {
        layout.facade_shell.as_ref().is_some_and(|shell| {
            shell.kind() == FacadeKind::Product && shell.ordered_keys().len() == facade_slot_count
        })
    } else {
        layout.facade_shell.is_none()
    };
    if !facade_shell_matches {
        return Err("a function facade stage shell disagrees with its semantic slots");
    }
    for (index, source) in layout.source_params.iter().enumerate() {
        let final_param = index + 1 == layout.body_abi_arity;
        let expected = if final_param {
            index..facade_slot_count
        } else {
            index..index + 1
        };
        let width = expected.end - expected.start;
        let expected_adapter = if width == 1 {
            CallableSourceParamAdapter::Identity
        } else {
            CallableSourceParamAdapter::RightNest
        };
        let shell_matches_adapter = match source.adapter {
            CallableSourceParamAdapter::RightNest => {
                source.product_shell.as_ref().is_some_and(|shell| {
                    shell.kind() == FacadeKind::Product && shell.ordered_keys().len() == width
                })
            }
            CallableSourceParamAdapter::UnitValue | CallableSourceParamAdapter::Identity => {
                source.product_shell.is_none()
            }
        };
        if source.facade_slots != expected
            || source.adapter != expected_adapter
            || !shell_matches_adapter
        {
            return Err("a function execution source range disagrees with its ABI partition");
        }
    }
    Ok(())
}

/// One declaration-owned stage in a callable's private execution layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CallableExecutionStage {
    Type { action: CallableTypeStageAction },
    Value(CallableValueStageLayout),
}

/// Compiler-private execution topology paired with one semantic facade plan.
///
/// It is derived once from the authoritative raw source heads and the same
/// deeply canonicalized scheme used by the facade plan. It is not part of a
/// facade use, public shell identity, codec, or equality decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallableExecutionLayout {
    presentation: CallablePresentationLayout,
    direct_projector_compactable_foralls: BTreeSet<FacadeUseId>,
}

impl CallableExecutionLayout {
    fn from_authoritative(
        callable: &AuthoritativeBoundaryCallable,
        plan: &BoundaryCallablePlan,
        nominals: &BoundaryNominalDependencies,
        root_uses: BoundaryFacadeExecutionPlan,
        transparent_payloads: BTreeMap<QualifiedTypeName, BoundaryFacadeExecutionPlan>,
    ) -> Self {
        let presentation = CallablePresentationLayout::from_authoritative(
            callable,
            plan,
            nominals,
            root_uses,
            transparent_payloads,
        );
        let execution = Self {
            presentation,
            direct_projector_compactable_foralls: direct_projector_compactable_foralls(
                callable, plan, nominals,
            ),
        };
        execution.assert_complete_alignment(plan, nominals);
        execution
    }

    pub(crate) fn head_stages(&self) -> &[CallableExecutionStage] {
        self.presentation.head_stages()
    }

    /// Per-use actions for the complete root facade. Declaration-head
    /// consumers peel `head_stages` and begin generic traversal at
    /// `BoundaryCallablePlan::entry().returned`, so the corresponding leading
    /// entries in this table are not consumed twice.
    pub(crate) fn root_uses(&self) -> &BoundaryFacadeExecutionPlan {
        self.presentation.root_uses()
    }

    pub(crate) fn transparent_payload(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<&BoundaryFacadeExecutionPlan> {
        self.presentation.transparent_payload(name)
    }

    /// Synthetic polymorphic stages that a statically known direct
    /// existential projector may compact in its private realization.
    ///
    /// The semantic facade keeps these `Forall` uses. This exact-ID set only
    /// prevents a backend from rediscovering their owner-derived positions by
    /// walking the plan or recounting declaration binders.
    pub(crate) fn direct_projector_compactable_foralls(&self) -> &BTreeSet<FacadeUseId> {
        &self.direct_projector_compactable_foralls
    }

    #[cfg(test)]
    pub(crate) fn transparent_payloads(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&QualifiedTypeName, &BoundaryFacadeExecutionPlan)>
    + ExactSizeIterator {
        self.presentation.transparent_payloads.iter()
    }

    fn assert_complete_alignment(
        &self,
        plan: &BoundaryCallablePlan,
        nominals: &BoundaryNominalDependencies,
    ) {
        self.presentation.assert_alignment(plan, nominals);
        for (index, use_) in plan.facade().uses().iter().enumerate() {
            if matches!(use_, FacadeUse::Forall { .. }) {
                assert!(matches!(
                    self.root_uses().use_at(FacadeUseId(index)),
                    BoundaryFacadeExecutionUse::InvokeForall
                ));
            }
        }
        for use_id in &self.direct_projector_compactable_foralls {
            assert!(matches!(
                plan.facade().use_at(*use_id),
                FacadeUse::Forall { .. }
            ));
            assert!(matches!(
                self.root_uses().use_at(*use_id),
                BoundaryFacadeExecutionUse::InvokeForall
            ));
        }
        let head_slot_count = plan
            .entry()
            .head_stages
            .iter()
            .map(|stage| match stage {
                BoundaryCallableHeadStage::Type { .. } => 0,
                BoundaryCallableHeadStage::Value { slots } => slots.len(),
            })
            .sum::<usize>();
        let head_shell_matches = if head_slot_count > 1 {
            plan.head_value_shell().is_some_and(|shell| {
                shell.kind() == FacadeKind::Product && shell.ordered_keys().len() == head_slot_count
            })
        } else {
            plan.head_value_shell().is_none()
        };
        assert!(head_shell_matches);
        let expected = nominals
            .declarations()
            .filter_map(|(name, declaration)| match declaration {
                BoundaryNominalDeclaration::Newtype {
                    transparent_payload: Some(_),
                    ..
                } => Some(name.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            self.presentation
                .transparent_payloads
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            expected,
            "live transparent execution keys equal semantic Both payload keys"
        );
        for (name, execution) in &self.presentation.transparent_payloads {
            let Some(BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            }) = nominals.declaration(name)
            else {
                unreachable!("a live transparent execution key denotes a Both payload")
            };
            execution
                .validate_alignment(payload.facade())
                .unwrap_or_else(|reason| {
                    unreachable!("live transparent execution alignment: {reason}")
                });
            let declarations = leading_declaration_binder_uses(payload).collect::<BTreeSet<_>>();
            assert_eq!(declarations.len(), payload.declaration_binders().len());
            for (index, use_) in payload.facade().uses().iter().enumerate() {
                if !matches!(use_, FacadeUse::Forall { .. }) {
                    continue;
                }
                let use_id = FacadeUseId(index);
                let expected = if declarations.contains(&use_id) {
                    BoundaryFacadeExecutionUse::DeclarationBinder
                } else {
                    BoundaryFacadeExecutionUse::InvokeForall
                };
                assert_eq!(execution.use_at(use_id), &expected);
            }
        }
    }
}

fn direct_projector_compactable_foralls(
    callable: &AuthoritativeBoundaryCallable,
    plan: &BoundaryCallablePlan,
    nominals: &BoundaryNominalDependencies,
) -> BTreeSet<FacadeUseId> {
    let BoundaryFacadeSiteOwner::NewtypeProjector { newtype, .. } = callable.site.owner() else {
        return BTreeSet::new();
    };
    let Some(name) =
        QualifiedTypeName::new(callable.site.module_segments().to_vec(), newtype.clone())
    else {
        unreachable!("a prepared projector has a qualified newtype identity")
    };
    let Some(BoundaryNominalDeclaration::Newtype {
        existential_params, ..
    }) = nominals.declaration(&name)
    else {
        unreachable!("a prepared projector retains its exact newtype declaration")
    };
    if existential_params.is_empty() {
        return BTreeSet::new();
    }

    let facade = plan.facade();
    let mut compactable = BTreeSet::new();
    let FacadeUse::Forall {
        result: returned, ..
    } = facade.use_at(plan.entry().returned)
    else {
        unreachable!("an existential projector returns its result-polymorphic CPS stage")
    };
    compactable.insert(plan.entry().returned);
    let FacadeUse::Function { slots, .. } = facade.use_at(*returned) else {
        unreachable!("an existential projector returns its continuation function")
    };
    let Some(mut continuation) = slots.first().copied() else {
        unreachable!("an existential projector has one continuation value")
    };
    for _ in existential_params {
        let FacadeUse::Forall { result, .. } = facade.use_at(continuation) else {
            unreachable!("an existential projector continuation binds every hidden type")
        };
        compactable.insert(continuation);
        continuation = *result;
    }
    compactable
}

/// Retirement facts carried by one plan-only retained host-function root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetainedBoundaryCallableMetadata {
    removed_at_version: u32,
    presentation: CallablePresentationLayout,
}

impl RetainedBoundaryCallableMetadata {
    pub(crate) fn removed_at_version(&self) -> u32 {
        self.removed_at_version
    }

    pub(crate) fn presentation(&self) -> &CallablePresentationLayout {
        &self.presentation
    }
}

/// Storage-level origin of one prepared callable.
///
/// A live callable owns the execution topology derived from its extant Routed
/// declaration. A retained callable owns retirement metadata and a
/// non-executable presentation only. Keeping the variants disjoint makes a
/// fabricated runtime layout for removed source unrepresentable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PreparedBoundaryCallableOrigin {
    Live(CallableExecutionLayout),
    Retained(RetainedBoundaryCallableMetadata),
}

/// Borrowed origin projection for backend consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparedBoundaryCallableOriginRef<'a> {
    Live(&'a CallableExecutionLayout),
    Retained(&'a RetainedBoundaryCallableMetadata),
}

/// One prepared plan and its origin-specific private data.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedBoundaryCallable {
    plan: BoundaryCallablePlan,
    nominals: BoundaryNominalDependencies,
    origin: PreparedBoundaryCallableOrigin,
}

/// Deterministic storage owned by the package-complete preparation transaction.
///
/// The catalog is private so a semantic plan cannot be detached from the
/// origin-specific data that shares its exact site identity.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BoundaryFacadeCatalog {
    sites: BTreeMap<BoundaryFacadeSiteId, PreparedBoundaryCallable>,
    shells: BTreeMap<FacadeShellId, BoundaryFacadeSupportOrigin>,
    public_newtypes: BTreeMap<QualifiedTypeName, BoundaryPublicNewtypeInventoryEntry>,
    retained_public_newtypes: BTreeMap<QualifiedTypeName, BoundaryPublicNewtypeInventoryEntry>,
    retained_public_newtype_versions: BTreeMap<QualifiedTypeName, u32>,
    host_bindings: BTreeMap<QualifiedTypeName, BoundaryHostBinding>,
    suppressed_retained_nominals: BTreeSet<QualifiedTypeName>,
    retained_live_nominals: BTreeMap<QualifiedTypeName, NominalCompatibilitySnapshot>,
}

impl BoundaryFacadeCatalog {
    fn empty() -> Self {
        Self {
            sites: BTreeMap::new(),
            shells: BTreeMap::new(),
            public_newtypes: BTreeMap::new(),
            retained_public_newtypes: BTreeMap::new(),
            retained_public_newtype_versions: BTreeMap::new(),
            host_bindings: BTreeMap::new(),
            suppressed_retained_nominals: BTreeSet::new(),
            retained_live_nominals: BTreeMap::new(),
        }
    }
}

/// One borrowed semantic plan and its origin-specific private data.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PreparedBoundaryCallableSite<'a> {
    site: &'a BoundaryFacadeSiteId,
    callable: &'a PreparedBoundaryCallable,
}

impl<'a> PreparedBoundaryCallableSite<'a> {
    pub(crate) fn site(self) -> &'a BoundaryFacadeSiteId {
        self.site
    }

    pub(crate) fn plan(self) -> &'a BoundaryCallablePlan {
        &self.callable.plan
    }

    pub(crate) fn nominals(self) -> &'a BoundaryNominalDependencies {
        &self.callable.nominals
    }

    pub(crate) fn origin(self) -> PreparedBoundaryCallableOriginRef<'a> {
        match &self.callable.origin {
            PreparedBoundaryCallableOrigin::Live(execution) => {
                PreparedBoundaryCallableOriginRef::Live(execution)
            }
            PreparedBoundaryCallableOrigin::Retained(metadata) => {
                PreparedBoundaryCallableOriginRef::Retained(metadata)
            }
        }
    }

    pub(crate) fn execution(self) -> Option<&'a CallableExecutionLayout> {
        match self.origin() {
            PreparedBoundaryCallableOriginRef::Live(execution) => Some(execution),
            PreparedBoundaryCallableOriginRef::Retained(_) => None,
        }
    }

    /// Exact source presentation for both live and retained callables.
    /// Retained presentations have no body execution or package-routing data.
    pub(crate) fn presentation(self) -> &'a CallablePresentationLayout {
        match self.origin() {
            PreparedBoundaryCallableOriginRef::Live(execution) => &execution.presentation,
            PreparedBoundaryCallableOriginRef::Retained(metadata) => metadata.presentation(),
        }
    }

    pub(crate) fn retained(self) -> Option<&'a RetainedBoundaryCallableMetadata> {
        match self.origin() {
            PreparedBoundaryCallableOriginRef::Live(_) => None,
            PreparedBoundaryCallableOriginRef::Retained(metadata) => Some(metadata),
        }
    }
}

/// Package-complete live and retained callable sites, keyed by exact identity.
///
/// Live collection derives its semantic-nominal snapshot from the same bridged
/// public newtype inventory that supplies constructor and projector sites.
/// Every retained host function instead receives the semantic nominals and
/// alias environment from only its own frozen type closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedBoundaryCallableSites {
    catalog: BoundaryFacadeCatalog,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LiveBoundaryCollectionWork {
    public_newtype_surface_builds: usize,
    nominal_declaration_index_builds: usize,
    nominal_declaration_index_item_visits: usize,
    callable_inventory_item_visits: usize,
    direct_callable_preparations: usize,
    by_name_callable_resolutions: usize,
    nominal_declaration_lookups: usize,
}

#[cfg(test)]
thread_local! {
    static LIVE_BOUNDARY_COLLECTION_WORK: std::cell::Cell<Option<LiveBoundaryCollectionWork>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn record_live_boundary_collection_work(update: impl FnOnce(&mut LiveBoundaryCollectionWork)) {
    LIVE_BOUNDARY_COLLECTION_WORK.with(|slot| {
        let Some(mut work) = slot.get() else {
            return;
        };
        update(&mut work);
        slot.set(Some(work));
    });
}

impl PreparedBoundaryCallableSites {
    pub(crate) fn collect(
        package: &Package<Routed>,
        replayed: Option<&ReplayedInterface>,
    ) -> Result<Self, BoundaryFacadeCollectionError> {
        let bridged = crate::pass::resolve::bridged_module_paths(package);
        #[cfg(test)]
        record_live_boundary_collection_work(|work| work.public_newtype_surface_builds += 1);
        let public_newtypes = crate::pass::resolve::public_newtype_host_surfaces(package);
        let live_nominal_declarations =
            LiveNominalDeclarationIndex::build(package, &public_newtypes);
        let inventory = public_newtypes
            .iter()
            .map(|((module_path, name), indexed)| {
                let name = qualified_type_name(module_path, name);
                (
                    name.clone(),
                    BoundaryPublicNewtypeInventoryEntry {
                        name,
                        type_params: indexed
                            .declaration
                            .type_params
                            .iter()
                            .map(BoundaryNominalTypeParam::from_source)
                            .collect(),
                        existential_params: indexed
                            .declaration
                            .existential_params
                            .iter()
                            .map(BoundaryNominalTypeParam::from_source)
                            .collect(),
                        surface: BoundaryNewtypeSurface::from_host_surface(Some(&indexed.surface)),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let live_semantic_nominals = inventory.keys().cloned().collect();
        let mut collector = PreparedBoundaryCallableSitesCollector::with_public_newtypes(inventory);
        collector.catalog.host_bindings = collect_root_host_bindings(
            package,
            replayed,
            &mut collector.catalog.suppressed_retained_nominals,
        )?;

        let mut callables = Vec::new();
        for module_path in &bridged {
            let entry = package
                .module(module_path)
                .expect("a bridged module path denotes a package module");
            for item in &entry.module.items {
                #[cfg(test)]
                record_live_boundary_collection_work(|work| {
                    work.callable_inventory_item_visits += 1;
                });
                match item {
                    Item::HostFn(declaration) => {
                        callables.push(prepare_host_boundary_callable_from_declaration(
                            package,
                            entry,
                            declaration,
                        ))
                    }
                    Item::FnDef(declaration) if declaration.vis.is_exported() => {
                        callables.push(prepare_exported_boundary_callable_from_declaration(
                            package,
                            entry,
                            declaration,
                        ))
                    }
                    _ => {}
                }
            }
        }

        for ((module_path, _name), indexed) in &public_newtypes {
            let entry = package
                .module(module_path)
                .expect("a public newtype identity denotes a package module");
            match &indexed.surface {
                NewtypeHostSurface::Opaque => {}
                NewtypeHostSurface::Constructor { .. } => {
                    callables.push(prepare_newtype_member_boundary_callable_from_declaration(
                        package,
                        module_path,
                        entry,
                        indexed.declaration,
                        NewtypeMemberRole::Constructor,
                    )?)
                }
                NewtypeHostSurface::Projector { .. } => {
                    callables.push(prepare_newtype_member_boundary_callable_from_declaration(
                        package,
                        module_path,
                        entry,
                        indexed.declaration,
                        NewtypeMemberRole::Projector,
                    )?)
                }
                NewtypeHostSurface::Both { .. } => {
                    callables.push(prepare_newtype_member_boundary_callable_from_declaration(
                        package,
                        module_path,
                        entry,
                        indexed.declaration,
                        NewtypeMemberRole::Constructor,
                    )?);
                    callables.push(prepare_newtype_member_boundary_callable_from_declaration(
                        package,
                        module_path,
                        entry,
                        indexed.declaration,
                        NewtypeMemberRole::Projector,
                    )?);
                }
            }
        }

        callables.sort_by(|left, right| left.site.cmp(&right.site));
        for callable in callables {
            collector.insert_live(
                callable,
                &live_semantic_nominals,
                package,
                &live_nominal_declarations,
            )?;
        }

        if let Some(replayed) = replayed {
            let mut retained = Vec::new();
            for removed in &replayed.removed {
                if let Some(callable) = prepare_retained_boundary_callable(removed)? {
                    retained.push(callable);
                }
            }
            collector.preflight_retained_nominal_conflicts(&retained);
            for callable in retained {
                collector.insert_retained(callable)?;
            }
        }

        collector.finish()
    }

    #[cfg(test)]
    pub(crate) fn collect_live(
        package: &Package<Routed>,
    ) -> Result<Self, BoundaryFacadeCollectionError> {
        Self::collect(package, None)
    }

    #[cfg(test)]
    fn collect_live_with_work(
        package: &Package<Routed>,
    ) -> (
        Result<Self, BoundaryFacadeCollectionError>,
        LiveBoundaryCollectionWork,
    ) {
        LIVE_BOUNDARY_COLLECTION_WORK.with(|slot| {
            assert!(
                slot.replace(Some(LiveBoundaryCollectionWork::default()))
                    .is_none()
            );
            let result = Self::collect_live(package);
            let work = slot
                .replace(None)
                .expect("the live collection work ledger is active");
            (result, work)
        })
    }

    pub(crate) fn site(
        &self,
        site: &BoundaryFacadeSiteId,
    ) -> Option<PreparedBoundaryCallableSite<'_>> {
        self.catalog
            .sites
            .get_key_value(site)
            .map(|(site, callable)| PreparedBoundaryCallableSite { site, callable })
    }

    pub(crate) fn sites(
        &self,
    ) -> impl DoubleEndedIterator<Item = PreparedBoundaryCallableSite<'_>> + ExactSizeIterator {
        self.catalog
            .sites
            .iter()
            .map(|(site, callable)| PreparedBoundaryCallableSite { site, callable })
    }

    pub(crate) fn shells(
        &self,
    ) -> impl DoubleEndedIterator<Item = &FacadeShellId> + ExactSizeIterator {
        self.catalog.shells.keys()
    }

    pub(crate) fn shell_origins(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&FacadeShellId, BoundaryFacadeSupportOrigin)> + ExactSizeIterator
    {
        self.catalog
            .shells
            .iter()
            .map(|(shell, origin)| (shell, *origin))
    }

    pub(crate) fn public_newtypes(
        &self,
    ) -> impl DoubleEndedIterator<Item = &BoundaryPublicNewtypeInventoryEntry> + ExactSizeIterator
    {
        self.catalog.public_newtypes.values()
    }

    pub(crate) fn public_newtype(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<&BoundaryPublicNewtypeInventoryEntry> {
        self.catalog.public_newtypes.get(name)
    }

    /// Frozen nominal carriers needed only by retained host-source signatures.
    /// These entries never imply a live constructor or projector site.
    pub(crate) fn retained_public_newtypes(
        &self,
    ) -> impl DoubleEndedIterator<Item = &BoundaryPublicNewtypeInventoryEntry> + ExactSizeIterator
    {
        self.catalog.retained_public_newtypes.values()
    }

    pub(crate) fn retained_public_newtype(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<&BoundaryPublicNewtypeInventoryEntry> {
        self.catalog.retained_public_newtypes.get(name)
    }

    pub(crate) fn retained_public_newtype_removed_at(
        &self,
        name: &QualifiedTypeName,
    ) -> Option<u32> {
        self.catalog
            .retained_public_newtype_versions
            .get(name)
            .copied()
    }

    /// Canonically ordered package-root host-type bindings. Live and retained
    /// entries share one exact identity space; retained entries never enter a
    /// callable site's nominal dependency map.
    pub(crate) fn host_bindings(
        &self,
    ) -> impl DoubleEndedIterator<Item = &BoundaryHostBinding> + ExactSizeIterator {
        self.catalog.host_bindings.values()
    }

    pub(crate) fn host_binding(&self, name: &QualifiedTypeName) -> Option<&BoundaryHostBinding> {
        self.catalog.host_bindings.get(name)
    }
}

fn collect_root_host_bindings(
    package: &Package<Routed>,
    replayed: Option<&ReplayedInterface>,
    suppressed_retained_nominals: &mut BTreeSet<QualifiedTypeName>,
) -> Result<BTreeMap<QualifiedTypeName, BoundaryHostBinding>, BoundaryFacadeCollectionError> {
    let bridged = crate::pass::resolve::bridged_module_paths(package);
    let mut bindings = BTreeMap::new();
    for (module_path, entry) in package.modules() {
        if !bridged.contains(module_path) {
            continue;
        }
        for item in &entry.module.items {
            let Item::HostType(host) = item else {
                continue;
            };
            let name = qualified_type_name(module_path, &host.name);
            insert_host_binding(
                &mut bindings,
                suppressed_retained_nominals,
                BoundaryHostBinding {
                    name,
                    type_params: host
                        .type_params
                        .iter()
                        .map(BoundaryNominalTypeParam::from_source)
                        .collect(),
                    binding: host.role.map_or(BoundaryHostTypeBinding::Roleless, |role| {
                        BoundaryHostTypeBinding::Role(role.role)
                    }),
                    owned: host.owned,
                    origin: BoundaryHostBindingOrigin::Live,
                },
            )?;
        }
    }

    if let Some(replayed) = replayed {
        for removed in &replayed.removed {
            if removed.entry.side != ContractSide::Env {
                continue;
            }
            let SigItem::HostType(host) = &removed.frozen else {
                continue;
            };
            let name =
                qualified_type_name(&removed.entry.name.module_path, &removed.entry.name.leaf);
            insert_host_binding(
                &mut bindings,
                suppressed_retained_nominals,
                BoundaryHostBinding {
                    name,
                    type_params: host
                        .type_params
                        .iter()
                        .map(BoundaryNominalTypeParam::from_source)
                        .collect(),
                    binding: host.role.map_or(BoundaryHostTypeBinding::Roleless, |role| {
                        BoundaryHostTypeBinding::Role(role.role)
                    }),
                    owned: host.owned,
                    origin: BoundaryHostBindingOrigin::Retained {
                        removed_at_version: removed.removed_at_version,
                    },
                },
            )?;
        }

        // A removed host function can be the only retained declaration that
        // reaches one of its exact host types. Its frozen closure, rather than
        // the later live package, owns that binding identity and shape.
        for removed in &replayed.removed {
            let Some(closure) = &removed.frozen_type_closure else {
                continue;
            };
            for (qualified, declaration) in &closure.declarations {
                let FrozenTypeItem::HostType(host) = &declaration.declaration else {
                    continue;
                };
                let name = qualified_type_name(&qualified.module_path, &qualified.leaf);
                insert_host_binding(
                    &mut bindings,
                    suppressed_retained_nominals,
                    BoundaryHostBinding {
                        name,
                        type_params: host
                            .type_params
                            .iter()
                            .map(BoundaryNominalTypeParam::from_source)
                            .collect(),
                        binding: host.role.map_or(BoundaryHostTypeBinding::Roleless, |role| {
                            BoundaryHostTypeBinding::Role(role.role)
                        }),
                        owned: host.owned,
                        origin: BoundaryHostBindingOrigin::Retained {
                            removed_at_version: removed.removed_at_version,
                        },
                    },
                )?;
            }
        }
    }

    Ok(bindings)
}

fn insert_host_binding(
    bindings: &mut BTreeMap<QualifiedTypeName, BoundaryHostBinding>,
    suppressed_retained_nominals: &mut BTreeSet<QualifiedTypeName>,
    incoming: BoundaryHostBinding,
) -> Result<(), BoundaryFacadeCollectionError> {
    if suppressed_retained_nominals.contains(incoming.name()) {
        if matches!(incoming.origin, BoundaryHostBindingOrigin::Live) {
            suppressed_retained_nominals.remove(incoming.name());
            bindings.insert(incoming.name().clone(), incoming);
        }
        return Ok(());
    }
    let Some(existing) = bindings.get_mut(incoming.name()) else {
        bindings.insert(incoming.name().clone(), incoming);
        return Ok(());
    };
    let same_params = existing.type_params.len() == incoming.type_params.len()
        && existing
            .type_params
            .iter()
            .zip(&incoming.type_params)
            .all(|(left, right)| left.kind == right.kind);
    if !same_params || existing.binding != incoming.binding {
        match (existing.origin, incoming.origin) {
            (BoundaryHostBindingOrigin::Live, BoundaryHostBindingOrigin::Retained { .. }) => {
                // A current declaration owns this exact identity. History
                // cannot make the live contract invalid or replace its
                // representation requirements.
                return Ok(());
            }
            (BoundaryHostBindingOrigin::Retained { .. }, BoundaryHostBindingOrigin::Live) => {
                *existing = incoming;
                return Ok(());
            }
            (
                BoundaryHostBindingOrigin::Retained { .. },
                BoundaryHostBindingOrigin::Retained { .. },
            ) => {
                let name = incoming.name.clone();
                bindings.remove(&name);
                suppressed_retained_nominals.insert(name);
                return Ok(());
            }
            (BoundaryHostBindingOrigin::Live, BoundaryHostBindingOrigin::Live) => {}
        }
        return Err(BoundaryFacadeCollectionError::InvalidHostBindingInventory {
            name: incoming.name,
            reason: "two same-provenance snapshots disagree on type parameters or role",
        });
    }

    existing.origin = match (existing.origin, incoming.origin) {
        (BoundaryHostBindingOrigin::Live, _) | (_, BoundaryHostBindingOrigin::Live) => {
            BoundaryHostBindingOrigin::Live
        }
        (
            BoundaryHostBindingOrigin::Retained {
                removed_at_version: left,
            },
            BoundaryHostBindingOrigin::Retained {
                removed_at_version: right,
            },
        ) => BoundaryHostBindingOrigin::Retained {
            removed_at_version: left.min(right),
        },
    };
    Ok(())
}

struct PreparedBoundaryCallableSitesCollector {
    catalog: BoundaryFacadeCatalog,
}

fn collect_presentation_shells(
    shells: &mut BTreeSet<FacadeShellId>,
    presentation: &CallablePresentationLayout,
) {
    fn collect_layout(shells: &mut BTreeSet<FacadeShellId>, layout: &CallableValueStageLayout) {
        shells.extend(layout.facade_shell().cloned());
        shells.extend(
            layout
                .source_params()
                .iter()
                .filter_map(|source| source.product_shell().cloned()),
        );
    }

    for stage in presentation.head_stages() {
        if let CallableExecutionStage::Value(layout) = stage {
            collect_layout(shells, layout);
        }
    }
    for use_ in &presentation.root_uses.uses {
        if let BoundaryFacadeExecutionUse::Function(layout) = use_ {
            collect_layout(shells, layout);
        }
    }
    for payload in presentation.transparent_payloads.values() {
        for use_ in &payload.uses {
            if let BoundaryFacadeExecutionUse::Function(layout) = use_ {
                collect_layout(shells, layout);
            }
        }
    }
}

fn prepared_callable_support_shells(
    callable: &PreparedBoundaryCallable,
) -> BTreeSet<FacadeShellId> {
    let mut shells = callable
        .plan
        .facade()
        .uses()
        .iter()
        .filter_map(|use_| match use_ {
            FacadeUse::Product { shell, .. } | FacadeUse::Sum { shell, .. } => Some(shell.clone()),
            FacadeUse::Function { shell, .. } => shell.clone(),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    shells.extend(callable.plan.head_value_shell().cloned());
    match &callable.origin {
        PreparedBoundaryCallableOrigin::Live(execution) => {
            collect_presentation_shells(&mut shells, &execution.presentation);
        }
        PreparedBoundaryCallableOrigin::Retained(metadata) => {
            collect_presentation_shells(&mut shells, &metadata.presentation);
        }
    }
    shells
}

impl PreparedBoundaryCallableSitesCollector {
    fn empty() -> Self {
        Self {
            catalog: BoundaryFacadeCatalog::empty(),
        }
    }

    fn with_public_newtypes(
        public_newtypes: BTreeMap<QualifiedTypeName, BoundaryPublicNewtypeInventoryEntry>,
    ) -> Self {
        let mut collector = Self::empty();
        collector.catalog.public_newtypes = public_newtypes;
        collector
    }

    fn preflight_retained_nominal_conflicts(
        &mut self,
        callables: &[PreparedRetainedBoundaryCallable],
    ) {
        // One public identity cannot expose two incompatible frozen epochs.
        // Suppress the optional compatibility closure before any site or
        // shell aggregation; see `specs/backends/README.md`
        // § Incompatible retained declaration epochs.
        let mut live_snapshots = BTreeMap::<QualifiedTypeName, NominalCompatibilitySnapshot>::new();
        for binding in self
            .catalog
            .host_bindings
            .values()
            .filter(|binding| matches!(binding.origin(), BoundaryHostBindingOrigin::Live))
        {
            live_snapshots.insert(
                binding.name().clone(),
                NominalCompatibilitySnapshot {
                    declaration: BoundaryNominalDeclaration::HostType {
                        type_params: binding.type_params().to_vec(),
                        binding: binding.binding(),
                    },
                    payload_known: true,
                },
            );
        }
        for (name, entry) in &self.catalog.public_newtypes {
            live_snapshots.insert(
                name.clone(),
                NominalCompatibilitySnapshot {
                    declaration: BoundaryNominalDeclaration::Newtype {
                        type_params: entry.type_params.clone(),
                        existential_params: entry.existential_params.clone(),
                        transparent_payload: None,
                        surface: entry.surface.clone(),
                    },
                    // An opaque declaration exposes only its header. Every
                    // transparent live payload is filled by the single site
                    // walk below, avoiding a newtype-by-site search.
                    payload_known: matches!(&entry.surface, BoundaryNewtypeSurface::Opaque),
                },
            );
        }
        for callable in self.catalog.sites.values() {
            if !matches!(callable.origin, PreparedBoundaryCallableOrigin::Live(_)) {
                continue;
            }
            for (name, declaration) in callable.nominals.declarations() {
                if let Some(snapshot) = live_snapshots.get_mut(name) {
                    if !snapshot.payload_known {
                        snapshot.declaration = declaration.clone();
                        snapshot.payload_known = true;
                    } else if !semantically_equal_nominal_declarations(
                        &snapshot.declaration,
                        declaration,
                    ) {
                        unreachable!(
                            "live prepared sites disagree on one exact nominal declaration"
                        );
                    }
                } else {
                    live_snapshots.insert(
                        name.clone(),
                        NominalCompatibilitySnapshot {
                            declaration: declaration.clone(),
                            payload_known: true,
                        },
                    );
                }
            }
        }
        let mut retained_snapshots =
            BTreeMap::<QualifiedTypeName, BoundaryNominalDeclaration>::new();
        for callable in callables {
            for (name, declaration) in callable.nominals.declarations() {
                if live_snapshots.contains_key(name) {
                    continue;
                }
                match retained_snapshots.get(name) {
                    Some(previous)
                        if !semantically_equal_nominal_declarations(previous, declaration) =>
                    {
                        self.catalog
                            .suppressed_retained_nominals
                            .insert(name.clone());
                    }
                    None => {
                        retained_snapshots.insert(name.clone(), declaration.clone());
                    }
                    Some(_) => {}
                }
            }
        }
        for name in &self.catalog.suppressed_retained_nominals {
            if self.catalog.host_bindings.get(name).is_some_and(|binding| {
                matches!(binding.origin(), BoundaryHostBindingOrigin::Retained { .. })
            }) {
                self.catalog.host_bindings.remove(name);
            }
        }
        self.catalog.retained_live_nominals = live_snapshots;
    }

    fn insert_live(
        &mut self,
        callable: AuthoritativeBoundaryCallable,
        semantic_nominals: &BTreeSet<QualifiedTypeName>,
        package: &Package<Routed>,
        nominal_declarations: &LiveNominalDeclarationIndex<'_>,
    ) -> Result<(), BoundaryFacadeCollectionError> {
        let site = callable.site.clone();
        let (plan, root_uses) = callable.semantic_plan(semantic_nominals)?;
        let live_nominals = live_nominal_dependencies(
            package,
            nominal_declarations,
            semantic_nominals,
            &site,
            &plan,
        )?;
        let execution = CallableExecutionLayout::from_authoritative(
            &callable,
            &plan,
            &live_nominals.semantic,
            root_uses,
            live_nominals.transparent_payloads,
        );
        assert_stage_alignment(&plan, &execution);
        self.insert_prepared(
            site,
            PreparedBoundaryCallable {
                plan,
                nominals: live_nominals.semantic,
                origin: PreparedBoundaryCallableOrigin::Live(execution),
            },
        )
    }

    fn insert_retained(
        &mut self,
        callable: PreparedRetainedBoundaryCallable,
    ) -> Result<(), BoundaryFacadeCollectionError> {
        if self
            .catalog
            .sites
            .get(&callable.site)
            .is_some_and(|existing| {
                matches!(existing.origin, PreparedBoundaryCallableOrigin::Live(_))
            })
        {
            // A current declaration at the same exact site owns the host
            // surface. Frozen history cannot add a second overload, mark the
            // live member deprecated, or retain dependencies reached only by
            // the superseded signature.
            return Ok(());
        }
        let removed_at_version = callable.metadata.removed_at_version();
        if callable.nominals.declarations().any(|(name, declaration)| {
            self.catalog.suppressed_retained_nominals.contains(name)
                || self
                    .catalog
                    .retained_live_nominals
                    .get(name)
                    .is_some_and(|live| {
                        !semantically_compatible_nominal_declarations(
                            &live.declaration,
                            live.payload_known,
                            declaration,
                            true,
                        )
                    })
        }) {
            return Ok(());
        }
        for (name, declaration) in callable.nominals.declarations() {
            let BoundaryNominalDeclaration::Newtype {
                type_params,
                existential_params,
                surface,
                ..
            } = declaration
            else {
                continue;
            };
            // A frozen signature may mention a transparent generic newtype
            // unsaturated, so retained support inventories every exposed
            // constructor identity, not only declarations whose saturated
            // values use a nominal carrier. Backends need that exact entry
            // to render the history-only type-constructor witness without
            // re-walking signature history. A live declaration still wins.
            if matches!(surface, BoundaryNewtypeSurface::Unexposed)
                || self.catalog.public_newtypes.contains_key(name)
            {
                continue;
            }
            let entry = BoundaryPublicNewtypeInventoryEntry {
                name: name.clone(),
                type_params: type_params.clone(),
                existential_params: existential_params.clone(),
                surface: surface.clone(),
            };
            if let Some(previous) = self.catalog.retained_public_newtypes.get(name) {
                let same_params =
                    |left: &[BoundaryNominalTypeParam], right: &[BoundaryNominalTypeParam]| {
                        left.len() == right.len()
                            && left
                                .iter()
                                .zip(right)
                                .all(|(left, right)| left.kind == right.kind)
                    };
                if !same_params(previous.type_params(), entry.type_params())
                    || !same_params(previous.existential_params(), entry.existential_params())
                    || previous.surface() != entry.surface()
                {
                    self.catalog.retained_public_newtypes.remove(name);
                    self.catalog.retained_public_newtype_versions.remove(name);
                    self.catalog
                        .suppressed_retained_nominals
                        .insert(name.clone());
                    continue;
                }
            } else {
                self.catalog
                    .retained_public_newtypes
                    .insert(name.clone(), entry);
            }
            self.catalog
                .retained_public_newtype_versions
                .entry(name.clone())
                .and_modify(|version| *version = (*version).min(removed_at_version))
                .or_insert(removed_at_version);
        }
        self.insert_prepared(
            callable.site,
            PreparedBoundaryCallable {
                plan: callable.plan,
                nominals: callable.nominals,
                origin: PreparedBoundaryCallableOrigin::Retained(callable.metadata),
            },
        )
    }

    fn insert_prepared(
        &mut self,
        site: BoundaryFacadeSiteId,
        callable: PreparedBoundaryCallable,
    ) -> Result<(), BoundaryFacadeCollectionError> {
        if self.catalog.sites.contains_key(&site) {
            return Err(BoundaryFacadeSiteConflict {
                #[cfg(test)]
                site,
            }
            .into());
        }

        let origin = match &callable.origin {
            PreparedBoundaryCallableOrigin::Live(_) => BoundaryFacadeSupportOrigin::Live,
            PreparedBoundaryCallableOrigin::Retained(metadata) => {
                BoundaryFacadeSupportOrigin::Retained {
                    removed_at_version: metadata.removed_at_version(),
                }
            }
        };
        for shell in prepared_callable_support_shells(&callable) {
            self.catalog
                .shells
                .entry(shell)
                .and_modify(|existing| *existing = existing.join(origin))
                .or_insert(origin);
        }
        let replaced = self.catalog.sites.insert(site, callable);
        assert!(replaced.is_none());
        Ok(())
    }

    fn finish(self) -> Result<PreparedBoundaryCallableSites, BoundaryFacadeCollectionError> {
        self.validate_public_newtype_inventory()?;
        for callable in self.catalog.sites.values() {
            if let PreparedBoundaryCallableOrigin::Live(execution) = &callable.origin {
                assert_stage_alignment(&callable.plan, execution);
                execution.assert_complete_alignment(&callable.plan, &callable.nominals);
            }
        }
        Ok(PreparedBoundaryCallableSites {
            catalog: self.catalog,
        })
    }

    fn validate_public_newtype_inventory(&self) -> Result<(), BoundaryFacadeCollectionError> {
        let mut expected_members = BTreeMap::new();
        for (name, entry) in &self.catalog.public_newtypes {
            if name != entry.name() {
                return Err(invalid_public_newtype_inventory(
                    name,
                    "the inventory key differs from the structured declaration identity",
                ));
            }

            let members = match entry.surface() {
                BoundaryNewtypeSurface::Unexposed => {
                    return Err(invalid_public_newtype_inventory(
                        name,
                        "an unexposed declaration cannot occur in the live public inventory",
                    ));
                }
                BoundaryNewtypeSurface::Opaque => Vec::new(),
                BoundaryNewtypeSurface::Constructor { member } => {
                    vec![(NewtypeMemberRole::Constructor, member.as_str())]
                }
                BoundaryNewtypeSurface::Projector { member } => {
                    vec![(NewtypeMemberRole::Projector, member.as_str())]
                }
                BoundaryNewtypeSurface::Both {
                    constructor,
                    projector,
                } => vec![
                    (NewtypeMemberRole::Constructor, constructor.as_str()),
                    (NewtypeMemberRole::Projector, projector.as_str()),
                ],
            };

            for (role, member) in members {
                let site = public_newtype_member_site(name, member, role);
                let replaced = expected_members.insert(site, name);
                if replaced.is_some() {
                    return Err(invalid_public_newtype_inventory(
                        name,
                        "two public inventory entries select the same callable site",
                    ));
                }
            }
        }

        for (site, name) in &expected_members {
            let Some(callable) = self.catalog.sites.get(site) else {
                return Err(invalid_public_newtype_inventory(
                    name,
                    "a required constructor or projector callable site is missing",
                ));
            };
            if !matches!(&callable.origin, PreparedBoundaryCallableOrigin::Live(_)) {
                return Err(invalid_public_newtype_inventory(
                    name,
                    "a public newtype member callable is not live",
                ));
            }
        }

        for (site, callable) in &self.catalog.sites {
            let newtype = match site.owner() {
                BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, .. }
                | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, .. } => newtype,
                BoundaryFacadeSiteOwner::HostFunction { .. }
                | BoundaryFacadeSiteOwner::ExportedFunction { .. } => continue,
            };
            let name = QualifiedTypeName::new(site.module_segments().to_vec(), newtype.clone())
                .expect("a collected site has a valid structured identity");
            let Some(entry) = self.catalog.public_newtypes.get(&name) else {
                return Err(invalid_public_newtype_inventory(
                    &name,
                    "a newtype member callable has no public inventory entry",
                ));
            };
            if expected_members.get(site).copied() != Some(entry.name()) {
                return Err(invalid_public_newtype_inventory(
                    &name,
                    "a newtype member callable does not match the inventoried surface",
                ));
            }
            if !matches!(&callable.origin, PreparedBoundaryCallableOrigin::Live(_)) {
                return Err(invalid_public_newtype_inventory(
                    &name,
                    "a retained callable cannot claim a live newtype-member identity",
                ));
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NominalCompatibilitySnapshot {
    declaration: BoundaryNominalDeclaration,
    payload_known: bool,
}

fn semantically_compatible_nominal_declarations(
    left: &BoundaryNominalDeclaration,
    left_payload_known: bool,
    right: &BoundaryNominalDeclaration,
    right_payload_known: bool,
) -> bool {
    let header_equal = semantically_equal_nominal_headers(left, right);
    if !header_equal || !left_payload_known || !right_payload_known {
        return header_equal;
    }
    semantically_equal_nominal_declarations(left, right)
}

fn public_newtype_member_site(
    name: &QualifiedTypeName,
    member: &str,
    role: NewtypeMemberRole,
) -> BoundaryFacadeSiteId {
    let owner = match role {
        NewtypeMemberRole::Constructor => BoundaryFacadeSiteOwner::NewtypeConstructor {
            newtype: name.name().to_owned(),
            member: member.to_owned(),
        },
        NewtypeMemberRole::Projector => BoundaryFacadeSiteOwner::NewtypeProjector {
            newtype: name.name().to_owned(),
            member: member.to_owned(),
        },
    };
    BoundaryFacadeSiteId::new(name.module_segments().to_vec(), owner)
        .expect("a public newtype inventory identity is valid")
}

fn invalid_public_newtype_inventory(
    name: &QualifiedTypeName,
    reason: &'static str,
) -> BoundaryFacadeCollectionError {
    BoundaryFacadeCollectionError::InvalidPublicNewtypeInventory {
        name: name.clone(),
        reason,
    }
}

#[cfg(test)]
fn live_semantic_nominals(package: &Package<Routed>) -> BTreeSet<QualifiedTypeName> {
    crate::pass::resolve::public_newtype_host_surfaces(package)
        .keys()
        .map(|(module_path, name)| qualified_type_name(module_path, name))
        .collect()
}

#[cfg(test)]
fn live_nominal_declaration_index(package: &Package<Routed>) -> LiveNominalDeclarationIndex<'_> {
    let public_newtypes = crate::pass::resolve::public_newtype_host_surfaces(package);
    LiveNominalDeclarationIndex::build(package, &public_newtypes)
}

fn qualified_type_name(module_path: &str, name: &str) -> QualifiedTypeName {
    QualifiedTypeName::new(
        module_path.split('/').map(str::to_owned).collect(),
        name.to_owned(),
    )
    .expect("a resolved module path and declaration name are non-empty")
}

#[derive(Clone)]
enum LiveNominalDeclaration<'a> {
    HostType(&'a crate::ast::HostType<Routed>),
    TypeAlias,
    Newtype {
        declaration: &'a crate::ast::Newtype<Routed>,
        surface: BoundaryNewtypeSurface,
    },
}

enum IndexedLiveNominalDeclaration<'a> {
    Unique(LiveNominalDeclaration<'a>),
    Duplicate,
}

struct LiveNominalDeclarationIndex<'a> {
    declarations: BTreeMap<QualifiedTypeName, IndexedLiveNominalDeclaration<'a>>,
}

impl<'a> LiveNominalDeclarationIndex<'a> {
    fn build(
        package: &'a Package<Routed>,
        public_newtypes: &crate::pass::resolve::PublicNewtypeHostSurfaces<'a, Routed>,
    ) -> Self {
        #[cfg(test)]
        record_live_boundary_collection_work(|work| {
            work.nominal_declaration_index_builds += 1;
        });

        let mut declarations = BTreeMap::new();
        for (module_path, entry) in package.modules() {
            for item in &entry.module.items {
                #[cfg(test)]
                record_live_boundary_collection_work(|work| {
                    work.nominal_declaration_index_item_visits += 1;
                });
                if let Item::TypeRecGroup(group) = item {
                    for member in &group.members {
                        let (name, declaration) = match member {
                            crate::ast::TypeRecMember::TypeAlias(declaration) => {
                                (declaration.name.as_str(), LiveNominalDeclaration::TypeAlias)
                            }
                            crate::ast::TypeRecMember::Newtype(declaration) => {
                                let name = qualified_type_name(module_path, &declaration.name);
                                let surface = BoundaryNewtypeSurface::from_host_surface(
                                    public_newtypes
                                        .get(&(module_path.to_owned(), declaration.name.clone()))
                                        .map(|indexed| &indexed.surface),
                                );
                                let declaration = LiveNominalDeclaration::Newtype {
                                    declaration,
                                    surface,
                                };
                                match declarations.entry(name) {
                                    std::collections::btree_map::Entry::Vacant(entry) => {
                                        entry.insert(IndexedLiveNominalDeclaration::Unique(
                                            declaration,
                                        ));
                                    }
                                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                                        entry.insert(IndexedLiveNominalDeclaration::Duplicate);
                                    }
                                }
                                continue;
                            }
                            crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
                        };
                        let name = qualified_type_name(module_path, name);
                        match declarations.entry(name) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(IndexedLiveNominalDeclaration::Unique(declaration));
                            }
                            std::collections::btree_map::Entry::Occupied(mut entry) => {
                                entry.insert(IndexedLiveNominalDeclaration::Duplicate);
                            }
                        }
                    }
                    continue;
                }
                let (name, declaration) = match item {
                    Item::HostType(declaration) => (
                        declaration.name.as_str(),
                        LiveNominalDeclaration::HostType(declaration),
                    ),
                    Item::TypeAlias(declaration) => {
                        (declaration.name.as_str(), LiveNominalDeclaration::TypeAlias)
                    }
                    Item::Newtype(declaration) => {
                        let name = qualified_type_name(module_path, &declaration.name);
                        let surface = BoundaryNewtypeSurface::from_host_surface(
                            public_newtypes
                                .get(&(module_path.to_owned(), declaration.name.clone()))
                                .map(|indexed| &indexed.surface),
                        );
                        let declaration = LiveNominalDeclaration::Newtype {
                            declaration,
                            surface,
                        };
                        match declarations.entry(name) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(IndexedLiveNominalDeclaration::Unique(declaration));
                            }
                            std::collections::btree_map::Entry::Occupied(mut entry) => {
                                entry.insert(IndexedLiveNominalDeclaration::Duplicate);
                            }
                        }
                        continue;
                    }
                    Item::TypeRecGroup(_) => unreachable!("handled above"),
                    _ => continue,
                };
                let name = qualified_type_name(module_path, name);
                match declarations.entry(name) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(IndexedLiveNominalDeclaration::Unique(declaration));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        entry.insert(IndexedLiveNominalDeclaration::Duplicate);
                    }
                }
            }
        }
        Self { declarations }
    }

    fn get(&self, name: &QualifiedTypeName) -> Option<&IndexedLiveNominalDeclaration<'a>> {
        #[cfg(test)]
        record_live_boundary_collection_work(|work| {
            work.nominal_declaration_lookups += 1;
        });
        self.declarations.get(name)
    }
}

struct PreparedNominalDependencies {
    semantic: BoundaryNominalDependencies,
    transparent_payloads: BTreeMap<QualifiedTypeName, BoundaryFacadeExecutionPlan>,
}

fn live_nominal_dependencies(
    package: &Package<Routed>,
    nominal_declarations: &LiveNominalDeclarationIndex<'_>,
    semantic_nominals: &BTreeSet<QualifiedTypeName>,
    site: &BoundaryFacadeSiteId,
    plan: &BoundaryCallablePlan,
) -> Result<PreparedNominalDependencies, BoundaryFacadeCollectionError> {
    let mut transparent_payloads = BTreeMap::new();
    let semantic = build_nominal_dependencies(site, plan, |name| {
        let module_path = name.module_segments().join("/");
        let Some(indexed) = nominal_declarations.get(name) else {
            return Err(missing_nominal_dependency(site, name));
        };
        let IndexedLiveNominalDeclaration::Unique(declaration) = indexed else {
            return Err(BoundaryFacadeCollectionError::InvalidNominalDependency {
                site: site.clone(),
                name: format!("{module_path}.{}", name.name()),
                reason: "the live module contains duplicate type declaration identities",
            });
        };
        match declaration {
            LiveNominalDeclaration::HostType(declaration) => Ok(project_host_type(
                &declaration.type_params,
                declaration.role.map(|role| role.role),
            )),
            LiveNominalDeclaration::Newtype {
                declaration,
                surface,
            } => {
                let transparent_payload = if surface.uses_nominal_carrier() {
                    None
                } else {
                    let binder_names = declaration
                        .type_params
                        .iter()
                        .chain(&declaration.existential_params)
                        .map(|param| param.name.clone())
                        .collect::<Vec<_>>();
                    let binder_locals = binder_names.iter().cloned().collect();
                    let payload = crate::backends::skin::qualify_newtype_declaration_payload(
                        declaration,
                        &module_path,
                        package,
                    );
                    let payload =
                        canonicalize_authoritative_scheme(&payload, package, Some(&binder_locals));
                    let payload = erase_live_comptime_type_names(payload, binder_names);
                    let (payload, execution) = prepare_newtype_payload_plan_with_presentation(
                        payload,
                        declaration
                            .type_params
                            .iter()
                            .chain(&declaration.existential_params),
                        semantic_nominals,
                    )?;
                    let replaced = transparent_payloads.insert(name.clone(), execution);
                    assert!(replaced.is_none());
                    Some(payload)
                };
                Ok(project_newtype(
                    declaration,
                    surface.clone(),
                    transparent_payload,
                ))
            }
            LiveNominalDeclaration::TypeAlias => {
                Err(BoundaryFacadeCollectionError::UnexpectedAliasDependency {
                    site: Box::new(site.clone()),
                    name: name.clone(),
                })
            }
        }
    })?;
    let expected = semantic
        .declarations()
        .filter_map(|(name, declaration)| match declaration {
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(_),
                ..
            } => Some(name.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if transparent_payloads
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected
    {
        return Err(BoundaryFacadeCollectionError::InvalidNominalDependency {
            site: site.clone(),
            name: "<transparent-payload execution>".to_owned(),
            reason: "live transparent payload execution keys disagree with semantic projections",
        });
    }
    for (name, execution) in &transparent_payloads {
        let Some(BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        }) = semantic.declaration(name)
        else {
            unreachable!("a validated transparent execution key has a payload facade")
        };
        execution
            .validate_alignment(payload.facade())
            .map_err(
                |_| BoundaryFacadeCollectionError::InvalidNominalDependency {
                    site: site.clone(),
                    name: format!("{}.{}", name.module_segments().join("."), name.name()),
                    reason: "live transparent payload execution does not align with its facade",
                },
            )?;
    }
    Ok(PreparedNominalDependencies {
        semantic,
        transparent_payloads,
    })
}

fn erase_live_comptime_type_names(ty: Type<Routed>, mut scope: Vec<String>) -> Type<Routed> {
    fn erase(ty: Type<Routed>, scope: &mut Vec<String>) -> Type<Routed> {
        match ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                if args.is_empty()
                    && let [single] = segments.as_slice()
                    && !scope.iter().rev().any(|bound| bound == single.as_str())
                    && let Some(builtin) =
                        crate::comptime::ComptimeBuiltin::from_public_name(single.as_str())
                    && builtin.is_type_name()
                    && let Some(erasure) = builtin.runtime_erasure()
                {
                    return match erasure {
                        crate::comptime::ComptimeRuntimeErasure::Bottom => Type::Bottom { meta },
                        crate::comptime::ComptimeRuntimeErasure::Unit => Type::Unit { meta },
                    };
                }
                Type::Path {
                    segments,
                    args: args.into_iter().map(|arg| erase(arg, scope)).collect(),
                    meta,
                }
            }
            Type::Unit { meta } => Type::Unit { meta },
            Type::Bottom { meta } => Type::Bottom { meta },
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => Type::Function {
                param: Box::new(erase(*param, scope)),
                ret: Box::new(erase(*ret, scope)),
                meta,
                abi_arity,
                caps,
            },
            Type::Product { left, right, meta } => Type::Product {
                left: Box::new(erase(*left, scope)),
                right: Box::new(erase(*right, scope)),
                meta,
            },
            Type::Sum { left, right, meta } => Type::Sum {
                left: Box::new(erase(*left, scope)),
                right: Box::new(erase(*right, scope)),
                meta,
            },
            Type::Forall { param, body, meta } => {
                scope.push(param.name.clone());
                let body = erase(*body, scope);
                scope.pop();
                Type::Forall {
                    param,
                    body: Box::new(body),
                    meta,
                }
            }
            Type::LabelSugar { ext, .. } => match ext {},
            Type::Infer { ext, .. } => match ext {},
            Type::Goal { ext, .. } => match ext {},
        }
    }

    erase(ty, &mut scope)
}

fn build_nominal_dependencies(
    site: &BoundaryFacadeSiteId,
    plan: &BoundaryCallablePlan,
    mut project: impl FnMut(
        &QualifiedTypeName,
    ) -> Result<BoundaryNominalDeclaration, BoundaryFacadeCollectionError>,
) -> Result<BoundaryNominalDependencies, BoundaryFacadeCollectionError> {
    let plan_names = plan_nominal_names(plan);
    let mut pending = plan_names.clone();
    let mut declarations = BTreeMap::new();

    while let Some(name) = pending.iter().next().cloned() {
        pending.remove(&name);
        if declarations.contains_key(&name) {
            continue;
        }
        let declaration = project(&name)?;
        let nested = match &declaration {
            BoundaryNominalDeclaration::HostType { .. } => BTreeSet::new(),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload,
                ..
            } => transparent_payload
                .as_ref()
                .map_or_else(BTreeSet::new, |payload| {
                    facade_nominal_names(payload.facade())
                }),
        };
        declarations.insert(name, declaration);
        pending.extend(
            nested
                .into_iter()
                .filter(|nested| !declarations.contains_key(nested)),
        );
    }

    let dependencies = BoundaryNominalDependencies { declarations };
    for name in plan_names {
        if dependencies.declaration(&name).is_none() {
            return Err(missing_nominal_dependency(site, &name));
        }
    }
    for declaration in dependencies.declarations.values() {
        let BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        } = declaration
        else {
            continue;
        };
        for name in facade_nominal_names(payload.facade()) {
            if dependencies.declaration(&name).is_none() {
                return Err(missing_nominal_dependency(site, &name));
            }
        }
    }
    Ok(dependencies)
}

fn plan_nominal_names(plan: &BoundaryCallablePlan) -> BTreeSet<QualifiedTypeName> {
    facade_nominal_names(plan.facade())
}

fn facade_nominal_names(plan: &BoundaryFacadePlan) -> BTreeSet<QualifiedTypeName> {
    plan.uses()
        .iter()
        .filter_map(|use_| match use_ {
            FacadeUse::Nominal { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

fn project_host_type(
    type_params: &[crate::ast::TypeParam],
    role: Option<Role>,
) -> BoundaryNominalDeclaration {
    BoundaryNominalDeclaration::HostType {
        type_params: type_params
            .iter()
            .map(BoundaryNominalTypeParam::from_source)
            .collect(),
        binding: role.map_or(
            BoundaryHostTypeBinding::Roleless,
            BoundaryHostTypeBinding::Role,
        ),
    }
}

fn project_newtype<P: crate::ast::Phase>(
    declaration: &crate::ast::Newtype<P>,
    surface: BoundaryNewtypeSurface,
    transparent_payload: Option<BoundaryNewtypePayloadPlan>,
) -> BoundaryNominalDeclaration {
    debug_assert_eq!(
        transparent_payload.is_some(),
        !surface.uses_nominal_carrier(),
        "exactly a both-member newtype owns a transparent payload plan"
    );
    BoundaryNominalDeclaration::Newtype {
        type_params: declaration
            .type_params
            .iter()
            .map(BoundaryNominalTypeParam::from_source)
            .collect(),
        existential_params: declaration
            .existential_params
            .iter()
            .map(BoundaryNominalTypeParam::from_source)
            .collect(),
        transparent_payload,
        surface,
    }
}

fn prepare_newtype_payload_plan_with_presentation<'a>(
    payload: Type<Routed>,
    params: impl DoubleEndedIterator<Item = &'a crate::ast::TypeParam> + Clone,
    semantic_nominals: &BTreeSet<QualifiedTypeName>,
) -> Result<(BoundaryNewtypePayloadPlan, BoundaryFacadeExecutionPlan), BoundaryFacadeCollectionError>
{
    let declaration_params = params.clone().collect::<Vec<_>>();
    let span = payload.span();
    let scheme = params.rev().fold(payload, |body, param| Type::Forall {
        param: param.clone(),
        body: Box::new(body),
        meta: crate::ast::Meta::new(span),
    });
    let prepared = PreparedBoundaryScheme::try_new(&scheme, semantic_nominals)?;
    let (facade, execution) =
        BoundaryFacadePlan::from_prepared_scheme_with_execution(prepared, declaration_params.len());
    let payload = finish_newtype_payload_plan(facade, &declaration_params);
    for binder_use in leading_declaration_binder_uses(&payload) {
        assert!(matches!(
            execution.use_at(binder_use),
            BoundaryFacadeExecutionUse::DeclarationBinder
        ));
    }
    Ok((payload, execution))
}

fn finish_newtype_payload_plan(
    facade: BoundaryFacadePlan,
    declaration_params: &[&crate::ast::TypeParam],
) -> BoundaryNewtypePayloadPlan {
    let mut current = facade.root();
    let mut declaration_binders = Vec::with_capacity(declaration_params.len());
    for source in declaration_params {
        let FacadeUse::Forall { binder, result, .. } = facade.use_at(current) else {
            unreachable!("a synthetic declaration binder remains a leading forall")
        };
        let projected = facade.binder(*binder);
        assert_eq!(projected.name, source.name);
        assert_eq!(projected.kind, source.effective_kind());
        declaration_binders.push(*binder);
        current = *result;
    }
    let shells = facade
        .uses()
        .iter()
        .filter_map(|use_| match use_ {
            FacadeUse::Product { shell, .. } | FacadeUse::Sum { shell, .. } => Some(shell.clone()),
            _ => None,
        })
        .collect();
    BoundaryNewtypePayloadPlan {
        facade,
        shells,
        declaration_binders,
        payload_root: current,
    }
}

fn leading_declaration_binder_uses(
    payload: &BoundaryNewtypePayloadPlan,
) -> impl Iterator<Item = FacadeUseId> + '_ {
    let mut current = payload.facade.root();
    payload.declaration_binders.iter().map(move |_| {
        let use_id = current;
        let FacadeUse::Forall { result, .. } = payload.facade.use_at(current) else {
            unreachable!("a recorded declaration binder denotes a forall use")
        };
        current = *result;
        use_id
    })
}

fn missing_nominal_dependency(
    site: &BoundaryFacadeSiteId,
    name: &QualifiedTypeName,
) -> BoundaryFacadeCollectionError {
    BoundaryFacadeCollectionError::MissingNominalDependency {
        site: Box::new(site.clone()),
        name: name.clone(),
    }
}

struct PreparedRetainedBoundaryCallable {
    site: BoundaryFacadeSiteId,
    plan: BoundaryCallablePlan,
    nominals: BoundaryNominalDependencies,
    metadata: RetainedBoundaryCallableMetadata,
}

fn prepare_retained_boundary_callable(
    removed: &RemovedItem,
) -> Result<Option<PreparedRetainedBoundaryCallable>, BoundaryFacadeCollectionError> {
    let is_function_contract = matches!(&removed.entry.kind, ContractKind::Fn { .. });
    let frozen_host = match &removed.frozen {
        SigItem::HostFn(host) => Some(host),
        _ => None,
    };
    match (removed.entry.side, is_function_contract, frozen_host) {
        (ContractSide::Env, true, Some(host)) => {
            prepare_exact_retained_host_callable(removed, host).map(Some)
        }
        (ContractSide::Env, true, None) => Err(inconsistent_retained_root(
            removed,
            "an env-side function contract must freeze a host fn",
        )),
        (_, _, Some(_)) => Err(inconsistent_retained_root(
            removed,
            "a frozen host fn must be an env-side function contract",
        )),
        _ => Ok(None),
    }
}

fn prepare_exact_retained_host_callable(
    removed: &RemovedItem,
    host: &crate::ast::HostFn<Surface>,
) -> Result<PreparedRetainedBoundaryCallable, BoundaryFacadeCollectionError> {
    if host.name != removed.entry.name.leaf {
        return Err(inconsistent_retained_root(
            removed,
            "the frozen host-fn name differs from its contract identity",
        ));
    }
    let module_segments =
        retained_module_segments(&removed.entry.name.module_path).ok_or_else(|| {
            inconsistent_retained_root(
                removed,
                "the retained module path has an empty identity component",
            )
        })?;
    let site = BoundaryFacadeSiteId::new(
        module_segments,
        BoundaryFacadeSiteOwner::HostFunction {
            name: host.name.clone(),
        },
    )
    .ok_or_else(|| {
        inconsistent_retained_root(
            removed,
            "the retained callable has an invalid module or declaration identity",
        )
    })?;
    let closure = removed.frozen_type_closure.as_ref().ok_or_else(|| {
        inconsistent_retained_root(
            removed,
            "a frozen host fn has no version-exact type closure",
        )
    })?;

    let resolver = FrozenRootTypeResolver { closure };
    let ContractKind::Fn {
        signature: recorded_signature,
        pure,
    } = &removed.entry.kind
    else {
        return Err(inconsistent_retained_root(
            removed,
            "an accepted retained host fn lost its function contract kind",
        ));
    };
    if *pure {
        return Err(inconsistent_retained_root(
            removed,
            "an env-side host function cannot carry a pure export contract",
        ));
    }
    let Some(frozen_signature) =
        resolver.canonical_host_signature(host, &removed.entry.name.module_path, &removed.imports)
    else {
        return Err(inconsistent_retained_root(
            removed,
            "the frozen host-fn parameter groups do not match its parameters",
        ));
    };
    if recorded_signature != &frozen_signature {
        return Err(inconsistent_retained_root(
            removed,
            "the normalized function contract differs from the frozen host declaration",
        ));
    }

    let semantic_nominals = resolver.semantic_nominals()?;
    let mut bound = BTreeMap::new();
    let mut params = Vec::with_capacity(host.params.len());
    for (index, param) in host.params.iter().enumerate() {
        match param {
            crate::ast::HostFnParam::Type(param) => {
                bound.insert(param.name.clone(), param.effective_kind());
                params.push(crate::ast::SignatureParam::Type(param.clone()));
            }
            crate::ast::HostFnParam::Value(param) => {
                let ty = resolver.resolve_value_type(
                    &param.ty,
                    &removed.entry.name.module_path,
                    &removed.imports,
                    &bound,
                    &mut Vec::new(),
                )?;
                params.push(crate::ast::SignatureParam::Value(crate::ast::Param {
                    name: param.name.clone().unwrap_or_else(|| format!("_p{index}")),
                    ty: Some(ty),
                    pattern: (),
                    meta: crate::ast::Meta::new(param.meta.span),
                }));
            }
        }
    }
    let ret = resolver.resolve_value_type(
        &host.ret,
        &removed.entry.name.module_path,
        &removed.imports,
        &bound,
        &mut Vec::new(),
    )?;
    let signature = Signature::from_parts(params, host.param_groups.clone());
    let scheme = signature.signature_ty(ret.clone(), ret.span());
    resolver.require_callable_spine_kinds(
        &scheme,
        &BTreeMap::new(),
        &removed.entry.name.module_path,
    )?;
    let authoritative = AuthoritativeBoundaryCallable {
        site: site.clone(),
        #[cfg(test)]
        exact_scheme: scheme.clone(),
        semantic_scheme: scheme,
        source_head_stages: signature_source_head_stages(&signature),
        head_stage_kinds: signature_head_stage_kinds(&signature),
    };
    let (plan, root_presentation) = authoritative.semantic_plan(&semantic_nominals)?;
    let retained_nominals =
        retained_nominal_dependencies(&resolver, &semantic_nominals, &site, &plan)?;
    let presentation = CallablePresentationLayout::from_authoritative(
        &authoritative,
        &plan,
        &retained_nominals.semantic,
        root_presentation,
        retained_nominals.transparent_payloads,
    );
    Ok(PreparedRetainedBoundaryCallable {
        site,
        plan,
        nominals: retained_nominals.semantic,
        metadata: RetainedBoundaryCallableMetadata {
            removed_at_version: removed.removed_at_version,
            presentation,
        },
    })
}

fn inconsistent_retained_root(
    removed: &RemovedItem,
    reason: &'static str,
) -> BoundaryFacadeCollectionError {
    BoundaryFacadeCollectionError::InconsistentRetainedRoot {
        module_path: removed.entry.name.module_path.clone(),
        name: removed.entry.name.leaf.clone(),
        reason,
    }
}

fn retained_module_segments(module_path: &str) -> Option<Vec<String>> {
    let segments: Vec<String> = module_path.split('/').map(str::to_owned).collect();
    (!segments.is_empty() && segments.iter().all(|segment| !segment.is_empty())).then_some(segments)
}

struct FrozenRootTypeResolver<'a> {
    closure: &'a FrozenTypeClosure,
}

struct ResolvedRetainedType {
    ty: Type<Routed>,
    kind: Kind,
}

fn retained_host_groups_match(host: &crate::ast::HostFn<Surface>) -> bool {
    if host.param_groups.is_empty() {
        return true;
    }
    let mut offset = 0usize;
    for group in &host.param_groups {
        let len = match *group {
            crate::ast::SignatureGroupKind::Type { len }
            | crate::ast::SignatureGroupKind::Value { len } => len,
        };
        let Some(end) = offset.checked_add(len) else {
            return false;
        };
        if end > host.params.len() {
            return false;
        }
        let matches_group = host.params[offset..end].iter().all(|param| {
            matches!(
                (group, param),
                (
                    crate::ast::SignatureGroupKind::Type { .. },
                    crate::ast::HostFnParam::Type(_)
                ) | (
                    crate::ast::SignatureGroupKind::Value { .. },
                    crate::ast::HostFnParam::Value(_)
                )
            )
        });
        if !matches_group {
            return false;
        }
        offset = end;
    }
    offset == host.params.len()
}

impl FrozenRootTypeResolver<'_> {
    fn semantic_nominals(
        &self,
    ) -> Result<BTreeSet<QualifiedTypeName>, BoundaryFacadeCollectionError> {
        self.closure
            .declarations
            .iter()
            .filter_map(|(name, declaration)| {
                matches!(&declaration.declaration, FrozenTypeItem::Newtype(_))
                    .then_some(self.semantic_nominal(name, &declaration.declaration))
            })
            .collect()
    }

    fn semantic_nominal(
        &self,
        name: &QualifiedName,
        declaration: &FrozenTypeItem,
    ) -> Result<QualifiedTypeName, BoundaryFacadeCollectionError> {
        self.validate_declaration_name(name, declaration)?;
        let module_segments = retained_module_segments(&name.module_path).ok_or_else(|| {
            BoundaryFacadeCollectionError::InconsistentRetainedRoot {
                module_path: name.module_path.clone(),
                name: name.leaf.clone(),
                reason: "a frozen type declaration has an invalid module identity",
            }
        })?;
        QualifiedTypeName::new(module_segments, name.leaf.clone()).ok_or_else(|| {
            BoundaryFacadeCollectionError::InconsistentRetainedRoot {
                module_path: name.module_path.clone(),
                name: name.leaf.clone(),
                reason: "a frozen type declaration has an invalid declaration identity",
            }
        })
    }

    fn canonical_host_signature(
        &self,
        host: &crate::ast::HostFn<Surface>,
        module_path: &str,
        imports: &[Import],
    ) -> Option<String> {
        if !retained_host_groups_match(host) {
            return None;
        }
        let params = host
            .params
            .iter()
            .map(|param| match param {
                crate::ast::HostFnParam::Type(param) => {
                    crate::ast::SignatureParam::Type(param.clone())
                }
                crate::ast::HostFnParam::Value(param) => {
                    crate::ast::SignatureParam::Value(crate::ast::Param {
                        name: param.name.clone().unwrap_or_default(),
                        ty: Some(param.ty.clone()),
                        pattern: Default::default(),
                        meta: param.meta.clone(),
                    })
                }
            })
            .collect();
        let signature = Signature::from_parts(params, host.param_groups.clone());
        let scheme = signature.signature_ty(host.ret.clone(), Span::new(0, 0));
        Some(crate::sig::canonical_type(&scheme, &|head| {
            self.canonical_qualified_head(module_path, imports, head)
        }))
    }

    fn canonical_qualified_head(
        &self,
        module_path: &str,
        imports: &[Import],
        written: &[String],
    ) -> Vec<String> {
        let Some((first, tail)) = written.split_first() else {
            return Vec::new();
        };
        if let Some(mut imported) = retained_selective_import(imports, first) {
            imported.extend_from_slice(tail);
            return imported;
        }
        if let Some(mut imported) = retained_qualified_import(imports, first) {
            imported.extend_from_slice(tail);
            return imported;
        }
        if tail.is_empty()
            && self
                .closure
                .declarations
                .contains_key(&QualifiedName::new(module_path, first.clone()))
        {
            let mut qualified: Vec<String> = module_path.split('/').map(str::to_owned).collect();
            qualified.push(first.clone());
            return qualified;
        }
        written.to_vec()
    }

    fn resolve_value_type(
        &self,
        ty: &Type<Surface>,
        module_path: &str,
        imports: &[Import],
        bound: &BTreeMap<String, Kind>,
        alias_path: &mut Vec<QualifiedName>,
    ) -> Result<Type<Routed>, BoundaryFacadeCollectionError> {
        let resolved = self.resolve_type(ty, module_path, imports, bound, alias_path)?;
        self.require_kind(
            &resolved.kind,
            &Kind::Star,
            module_path,
            "a callable value parameter or result",
        )?;
        Ok(resolved.ty)
    }

    fn resolve_type(
        &self,
        ty: &Type<Surface>,
        module_path: &str,
        imports: &[Import],
        bound: &BTreeMap<String, Kind>,
        alias_path: &mut Vec<QualifiedName>,
    ) -> Result<ResolvedRetainedType, BoundaryFacadeCollectionError> {
        match ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                let args = args
                    .iter()
                    .map(|arg| self.resolve_type(arg, module_path, imports, bound, alias_path))
                    .collect::<Result<Vec<_>, _>>()?;
                if segments.len() == 1
                    && let Some(head_kind) = bound.get(segments[0].as_str())
                {
                    let kind = self.apply_kind(
                        head_kind.clone(),
                        &args,
                        module_path,
                        segments[0].as_str(),
                    )?;
                    return Ok(ResolvedRetainedType {
                        ty: Type::synth_path_segments(
                            segments.clone(),
                            args.into_iter().map(|arg| arg.ty).collect(),
                            meta.span,
                        ),
                        kind,
                    });
                }
                let written: Vec<String> = segments
                    .iter()
                    .map(|segment| segment.name.clone())
                    .collect();
                let explicit_import = written.first().is_some_and(|first| {
                    retained_selective_import(imports, first).is_some()
                        || retained_qualified_import(imports, first).is_some()
                });
                let Some(name) = self.qualify_type_name(module_path, imports, &written) else {
                    if !explicit_import
                        && args.is_empty()
                        && let [single] = written.as_slice()
                        && let Some(builtin) =
                            crate::comptime::ComptimeBuiltin::from_public_name(single)
                        && builtin.is_type_name()
                        && let Some(erasure) = builtin.runtime_erasure()
                    {
                        let ty = match erasure {
                            crate::comptime::ComptimeRuntimeErasure::Bottom => Type::Bottom {
                                meta: crate::ast::Meta::new(meta.span),
                            },
                            crate::comptime::ComptimeRuntimeErasure::Unit => Type::Unit {
                                meta: crate::ast::Meta::new(meta.span),
                            },
                        };
                        return Ok(ResolvedRetainedType {
                            ty,
                            kind: Kind::Star,
                        });
                    }
                    return Err(BoundaryFacadeCollectionError::UnresolvedRetainedType {
                        module_path: module_path.to_owned(),
                        path: written,
                    });
                };
                let declaration = self
                    .closure
                    .declarations
                    .get(&name)
                    .expect("a qualified retained name was proved present in its closure");
                self.validate_declaration_name(&name, &declaration.declaration)?;
                match &declaration.declaration {
                    FrozenTypeItem::HostType(host) => {
                        let head_kind = retained_kind_from_params(&host.type_params, Kind::Star);
                        let kind =
                            self.apply_kind(head_kind, &args, module_path, &name.to_string())?;
                        Ok(ResolvedRetainedType {
                            ty: retained_nominal_type(
                                &name,
                                args.into_iter().map(|arg| arg.ty).collect(),
                                meta.span,
                            ),
                            kind,
                        })
                    }
                    FrozenTypeItem::Newtype(newtype) => {
                        let head_kind = retained_kind_from_params(&newtype.type_params, Kind::Star);
                        let kind =
                            self.apply_kind(head_kind, &args, module_path, &name.to_string())?;
                        Ok(ResolvedRetainedType {
                            ty: retained_nominal_type(
                                &name,
                                args.into_iter().map(|arg| arg.ty).collect(),
                                meta.span,
                            ),
                            kind,
                        })
                    }
                    FrozenTypeItem::TypeAlias(alias) => {
                        if alias.type_params.len() != args.len() {
                            return Err(BoundaryFacadeCollectionError::RetainedAliasArity {
                                name,
                                expected: alias.type_params.len(),
                                actual: args.len(),
                            });
                        }
                        for (param, argument) in alias.type_params.iter().zip(&args) {
                            self.require_kind(
                                &argument.kind,
                                &param.effective_kind(),
                                module_path,
                                &format!("argument for retained alias parameter {:?}", param.name),
                            )?;
                        }
                        if let Some(cycle_start) =
                            alias_path.iter().position(|ancestor| ancestor == &name)
                        {
                            let mut path = alias_path[cycle_start..].to_vec();
                            path.push(name);
                            return Err(BoundaryFacadeCollectionError::RetainedAliasCycle { path });
                        }
                        alias_path.push(name.clone());
                        let alias_bound = alias
                            .type_params
                            .iter()
                            .map(|param| (param.name.clone(), param.effective_kind()))
                            .collect();
                        let body = self.resolve_type(
                            &alias.body,
                            &name.module_path,
                            &declaration.imports,
                            &alias_bound,
                            alias_path,
                        );
                        alias_path.pop();
                        let body = body?;
                        let substitution: HashMap<_, _> = alias
                            .type_params
                            .iter()
                            .map(|param| param.name.clone())
                            .zip(args.into_iter().map(|argument| argument.ty))
                            .collect();
                        let substituted =
                            substitute_retained_type(&body.ty, &substitution, module_path)?;
                        let substituted_kind =
                            self.kind_of_routed(&substituted, bound, module_path)?;
                        if substituted_kind != body.kind {
                            return Err(BoundaryFacadeCollectionError::InvalidRetainedKind {
                                module_path: module_path.to_owned(),
                                message: format!(
                                    "alias substitution changed kind from `{}` to \
                                     `{substituted_kind}`",
                                    body.kind
                                ),
                            });
                        }
                        Ok(ResolvedRetainedType {
                            ty: substituted,
                            kind: body.kind,
                        })
                    }
                }
            }
            Type::Unit { meta } => Ok(ResolvedRetainedType {
                ty: Type::Unit {
                    meta: crate::ast::Meta::new(meta.span),
                },
                kind: Kind::Star,
            }),
            Type::Bottom { meta } => Ok(ResolvedRetainedType {
                ty: Type::Bottom {
                    meta: crate::ast::Meta::new(meta.span),
                },
                kind: Kind::Star,
            }),
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                ..
            } => {
                let param = self.resolve_type(param, module_path, imports, bound, alias_path)?;
                let ret = self.resolve_type(ret, module_path, imports, bound, alias_path)?;
                self.require_kind(
                    &param.kind,
                    &Kind::Star,
                    module_path,
                    "a retained function parameter",
                )?;
                self.require_kind(
                    &ret.kind,
                    &Kind::Star,
                    module_path,
                    "a retained function result",
                )?;
                Ok(ResolvedRetainedType {
                    ty: Type::Function {
                        param: Box::new(param.ty),
                        ret: Box::new(ret.ty),
                        meta: crate::ast::Meta::new(meta.span),
                        abi_arity: *abi_arity,
                        caps: FnTypeCapabilities::default(),
                    },
                    kind: Kind::Star,
                })
            }
            Type::Product { left, right, meta } => {
                let left = self.resolve_type(left, module_path, imports, bound, alias_path)?;
                let right = self.resolve_type(right, module_path, imports, bound, alias_path)?;
                self.require_kind(
                    &left.kind,
                    &Kind::Star,
                    module_path,
                    "a retained product slot",
                )?;
                self.require_kind(
                    &right.kind,
                    &Kind::Star,
                    module_path,
                    "a retained product slot",
                )?;
                Ok(ResolvedRetainedType {
                    ty: Type::Product {
                        left: Box::new(left.ty),
                        right: Box::new(right.ty),
                        meta: crate::ast::Meta::new(meta.span),
                    },
                    kind: Kind::Star,
                })
            }
            Type::Sum { left, right, meta } => {
                let left = self.resolve_type(left, module_path, imports, bound, alias_path)?;
                let right = self.resolve_type(right, module_path, imports, bound, alias_path)?;
                self.require_kind(&left.kind, &Kind::Star, module_path, "a retained sum arm")?;
                self.require_kind(&right.kind, &Kind::Star, module_path, "a retained sum arm")?;
                Ok(ResolvedRetainedType {
                    ty: Type::Sum {
                        left: Box::new(left.ty),
                        right: Box::new(right.ty),
                        meta: crate::ast::Meta::new(meta.span),
                    },
                    kind: Kind::Star,
                })
            }
            Type::Forall { param, body, meta } => {
                let mut body_bound = bound.clone();
                body_bound.insert(param.name.clone(), param.effective_kind());
                let body =
                    self.resolve_type(body, module_path, imports, &body_bound, alias_path)?;
                self.require_kind(
                    &body.kind,
                    &Kind::Star,
                    module_path,
                    "a retained forall body",
                )?;
                if param.effective_kind() != Kind::Star
                    && !retained_complete_function_scheme(&body.ty)
                {
                    return Err(BoundaryFacadeCollectionError::InvalidRetainedKind {
                        module_path: module_path.to_owned(),
                        message: format!(
                            "higher-kinded forall binder {:?} does not bind a complete function \
                             scheme",
                            param.name
                        ),
                    });
                }
                Ok(ResolvedRetainedType {
                    ty: Type::Forall {
                        param: param.clone(),
                        body: Box::new(body.ty),
                        meta: crate::ast::Meta::new(meta.span),
                    },
                    kind: Kind::Star,
                })
            }
            Type::LabelSugar { .. } => {
                Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot {
                    module_path: module_path.to_owned(),
                    name: "<type>".to_owned(),
                    reason: "a frozen Kio' type contains surface label sugar",
                })
            }
            Type::Infer { .. } => Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot {
                module_path: module_path.to_owned(),
                name: "<type>".to_owned(),
                reason: "a frozen Kio' type contains an inference hole",
            }),
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn require_star_kind(
        &self,
        ty: &Type<Routed>,
        bound: &BTreeMap<String, Kind>,
        module_path: &str,
        context: &str,
    ) -> Result<(), BoundaryFacadeCollectionError> {
        let kind = self.kind_of_routed(ty, bound, module_path)?;
        self.require_kind(&kind, &Kind::Star, module_path, context)
    }

    fn require_callable_spine_kinds(
        &self,
        ty: &Type<Routed>,
        bound: &BTreeMap<String, Kind>,
        module_path: &str,
    ) -> Result<(), BoundaryFacadeCollectionError> {
        match ty {
            Type::Forall { param, body, .. } => {
                let mut body_bound = bound.clone();
                body_bound.insert(param.name.clone(), param.effective_kind());
                self.require_callable_spine_kinds(body, &body_bound, module_path)
            }
            Type::Function { param, ret, .. } => {
                self.require_star_kind(
                    param,
                    bound,
                    module_path,
                    "a retained callable-spine parameter",
                )?;
                self.require_callable_spine_kinds(ret, bound, module_path)
            }
            terminal => self.require_star_kind(
                terminal,
                bound,
                module_path,
                "a retained callable-spine result",
            ),
        }
    }

    fn kind_of_routed(
        &self,
        ty: &Type<Routed>,
        bound: &BTreeMap<String, Kind>,
        module_path: &str,
    ) -> Result<Kind, BoundaryFacadeCollectionError> {
        match ty {
            Type::Path { segments, args, .. } => {
                let head_kind = if segments.len() == 1 {
                    bound.get(segments[0].as_str()).cloned().ok_or_else(|| {
                        BoundaryFacadeCollectionError::InvalidRetainedKind {
                            module_path: module_path.to_owned(),
                            message: format!(
                                "expanded type retains unbound head {:?}",
                                segments[0].name
                            ),
                        }
                    })?
                } else {
                    let name =
                        QualifiedTypeName::from_path_segments(segments).ok_or_else(|| {
                            BoundaryFacadeCollectionError::InvalidRetainedKind {
                                module_path: module_path.to_owned(),
                                message: "expanded nominal identity is malformed".to_owned(),
                            }
                        })?;
                    let qualified = QualifiedName::new(
                        name.module_segments().join("/"),
                        name.name().to_owned(),
                    );
                    let declaration =
                        self.closure.declarations.get(&qualified).ok_or_else(|| {
                            BoundaryFacadeCollectionError::InvalidRetainedKind {
                                module_path: module_path.to_owned(),
                                message: format!(
                                    "expanded nominal {qualified} is absent from its frozen root"
                                ),
                            }
                        })?;
                    match &declaration.declaration {
                        FrozenTypeItem::HostType(host) => {
                            retained_kind_from_params(&host.type_params, Kind::Star)
                        }
                        FrozenTypeItem::Newtype(newtype) => {
                            retained_kind_from_params(&newtype.type_params, Kind::Star)
                        }
                        FrozenTypeItem::TypeAlias(_) => {
                            return Err(BoundaryFacadeCollectionError::InvalidRetainedKind {
                                module_path: module_path.to_owned(),
                                message: format!(
                                    "expanded callable scheme still contains alias {qualified}"
                                ),
                            });
                        }
                    }
                };
                let resolved_args = args
                    .iter()
                    .map(|arg| {
                        self.kind_of_routed(arg, bound, module_path).map(|kind| {
                            ResolvedRetainedType {
                                ty: arg.clone(),
                                kind,
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.apply_kind(
                    head_kind,
                    &resolved_args,
                    module_path,
                    &segments
                        .iter()
                        .map(crate::ast::PathSegment::as_str)
                        .collect::<Vec<_>>()
                        .join("."),
                )
            }
            Type::Unit { .. } | Type::Bottom { .. } => Ok(Kind::Star),
            Type::Function { param, ret, .. } => {
                self.require_star_kind(param, bound, module_path, "a retained function parameter")?;
                self.require_star_kind(ret, bound, module_path, "a retained function result")?;
                Ok(Kind::Star)
            }
            Type::Product { left, right, .. } => {
                self.require_star_kind(left, bound, module_path, "a retained product slot")?;
                self.require_star_kind(right, bound, module_path, "a retained product slot")?;
                Ok(Kind::Star)
            }
            Type::Sum { left, right, .. } => {
                self.require_star_kind(left, bound, module_path, "a retained sum arm")?;
                self.require_star_kind(right, bound, module_path, "a retained sum arm")?;
                Ok(Kind::Star)
            }
            Type::Forall { param, body, .. } => {
                let mut body_bound = bound.clone();
                body_bound.insert(param.name.clone(), param.effective_kind());
                self.require_star_kind(body, &body_bound, module_path, "a retained forall body")?;
                if param.effective_kind() != Kind::Star && !retained_complete_function_scheme(body)
                {
                    return Err(BoundaryFacadeCollectionError::InvalidRetainedKind {
                        module_path: module_path.to_owned(),
                        message: format!(
                            "higher-kinded forall binder {:?} does not bind a complete function \
                             scheme",
                            param.name
                        ),
                    });
                }
                Ok(Kind::Star)
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn apply_kind(
        &self,
        mut remaining: Kind,
        args: &[ResolvedRetainedType],
        module_path: &str,
        subject: &str,
    ) -> Result<Kind, BoundaryFacadeCollectionError> {
        for argument in args {
            let (domain, codomain) = match remaining {
                Kind::Arrow(domain, codomain) => (*domain, *codomain),
                Kind::Star => {
                    return Err(BoundaryFacadeCollectionError::InvalidRetainedKind {
                        module_path: module_path.to_owned(),
                        message: format!(
                            "type head {subject:?} has kind `*` and cannot receive another \
                             argument"
                        ),
                    });
                }
            };
            self.require_kind(
                &argument.kind,
                &domain,
                module_path,
                &format!("an argument to retained type head {subject:?}"),
            )?;
            remaining = codomain;
        }
        Ok(remaining)
    }

    fn require_kind(
        &self,
        actual: &Kind,
        expected: &Kind,
        module_path: &str,
        context: &str,
    ) -> Result<(), BoundaryFacadeCollectionError> {
        if actual == expected {
            Ok(())
        } else {
            Err(BoundaryFacadeCollectionError::InvalidRetainedKind {
                module_path: module_path.to_owned(),
                message: format!(
                    "{context} requires kind `{expected}`, but the frozen type has kind \
                     `{actual}`"
                ),
            })
        }
    }

    fn qualify_type_name(
        &self,
        module_path: &str,
        imports: &[Import],
        written: &[String],
    ) -> Option<QualifiedName> {
        let (first, tail) = written.split_first()?;
        if let Some(mut imported) = retained_selective_import(imports, first) {
            imported.extend_from_slice(tail);
            return qualified_name_from_segments(&imported)
                .filter(|name| self.closure.declarations.contains_key(name));
        }
        if let Some(mut imported) = retained_qualified_import(imports, first) {
            imported.extend_from_slice(tail);
            return qualified_name_from_segments(&imported)
                .filter(|name| self.closure.declarations.contains_key(name));
        }
        if tail.is_empty() {
            let name = QualifiedName::new(module_path, first.clone());
            return self
                .closure
                .declarations
                .contains_key(&name)
                .then_some(name);
        }
        qualified_name_from_segments(written)
            .filter(|name| self.closure.declarations.contains_key(name))
    }

    fn validate_declaration_name(
        &self,
        name: &QualifiedName,
        declaration: &FrozenTypeItem,
    ) -> Result<(), BoundaryFacadeCollectionError> {
        let declaration_name = match declaration {
            FrozenTypeItem::HostType(declaration) => &declaration.name,
            FrozenTypeItem::TypeAlias(declaration) => &declaration.name,
            FrozenTypeItem::Newtype(declaration) => &declaration.name,
        };
        if declaration_name == &name.leaf {
            Ok(())
        } else {
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot {
                module_path: name.module_path.clone(),
                name: name.leaf.clone(),
                reason: "a frozen type declaration name differs from its closure identity",
            })
        }
    }
}

fn retained_nominal_dependencies(
    resolver: &FrozenRootTypeResolver<'_>,
    semantic_nominals: &BTreeSet<QualifiedTypeName>,
    site: &BoundaryFacadeSiteId,
    plan: &BoundaryCallablePlan,
) -> Result<PreparedNominalDependencies, BoundaryFacadeCollectionError> {
    let mut transparent_payloads = BTreeMap::new();
    let semantic = build_nominal_dependencies(site, plan, |name| {
        let qualified =
            QualifiedName::new(name.module_segments().join("/"), name.name().to_owned());
        let Some(frozen) = resolver.closure.declarations.get(&qualified) else {
            return Err(missing_nominal_dependency(site, name));
        };
        resolver.validate_declaration_name(&qualified, &frozen.declaration)?;
        match &frozen.declaration {
            FrozenTypeItem::HostType(declaration) => Ok(project_host_type(
                &declaration.type_params,
                declaration.role.map(|role| role.role),
            )),
            FrozenTypeItem::Newtype(declaration) => {
                let host_surface = declaration.host_surface();
                let surface = BoundaryNewtypeSurface::from_host_surface(host_surface.as_ref());
                let transparent_payload = if surface.uses_nominal_carrier() {
                    None
                } else {
                    let bound = declaration
                        .type_params
                        .iter()
                        .chain(&declaration.existential_params)
                        .map(|param| (param.name.clone(), param.effective_kind()))
                        .collect();
                    let resolved = resolver.resolve_type(
                        &declaration.payload,
                        &qualified.module_path,
                        &frozen.imports,
                        &bound,
                        &mut Vec::new(),
                    )?;
                    resolver.require_kind(
                        &resolved.kind,
                        &Kind::Star,
                        &qualified.module_path,
                        "a retained newtype payload",
                    )?;
                    let (payload, presentation) = prepare_newtype_payload_plan_with_presentation(
                        resolved.ty,
                        declaration
                            .type_params
                            .iter()
                            .chain(&declaration.existential_params),
                        semantic_nominals,
                    )?;
                    let replaced = transparent_payloads.insert(name.clone(), presentation);
                    assert!(replaced.is_none());
                    Some(payload)
                };
                Ok(project_newtype(declaration, surface, transparent_payload))
            }
            FrozenTypeItem::TypeAlias(_) => {
                Err(BoundaryFacadeCollectionError::UnexpectedAliasDependency {
                    site: Box::new(site.clone()),
                    name: name.clone(),
                })
            }
        }
    })?;
    Ok(PreparedNominalDependencies {
        semantic,
        transparent_payloads,
    })
}

fn retained_kind_from_params(params: &[crate::ast::TypeParam], result: Kind) -> Kind {
    params.iter().rev().fold(result, |remaining, param| {
        Kind::Arrow(Box::new(param.effective_kind()), Box::new(remaining))
    })
}

fn retained_complete_function_scheme(ty: &Type<Routed>) -> bool {
    match ty {
        Type::Forall { body, .. } => retained_complete_function_scheme(body),
        Type::Function { .. } => true,
        _ => false,
    }
}

fn substitute_retained_type(
    ty: &Type<Routed>,
    substitution: &HashMap<String, Type<Routed>>,
    module_path: &str,
) -> Result<Type<Routed>, BoundaryFacadeCollectionError> {
    if substitution.is_empty() {
        return Ok(ty.clone());
    }
    match ty {
        Type::Unit { .. } | Type::Bottom { .. } => Ok(ty.clone()),
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => Ok(Type::Function {
            param: Box::new(substitute_retained_type(param, substitution, module_path)?),
            ret: Box::new(substitute_retained_type(ret, substitution, module_path)?),
            meta: crate::ast::Meta::new(meta.span),
            abi_arity: *abi_arity,
            caps: caps.clone(),
        }),
        Type::Product { left, right, meta } => Ok(Type::Product {
            left: Box::new(substitute_retained_type(left, substitution, module_path)?),
            right: Box::new(substitute_retained_type(right, substitution, module_path)?),
            meta: crate::ast::Meta::new(meta.span),
        }),
        Type::Sum { left, right, meta } => Ok(Type::Sum {
            left: Box::new(substitute_retained_type(left, substitution, module_path)?),
            right: Box::new(substitute_retained_type(right, substitution, module_path)?),
            meta: crate::ast::Meta::new(meta.span),
        }),
        Type::Path {
            segments,
            args,
            meta,
        } => {
            let args = args
                .iter()
                .map(|arg| substitute_retained_type(arg, substitution, module_path))
                .collect::<Result<Vec<_>, _>>()?;
            if segments.len() == 1
                && let Some(replacement) = substitution.get(segments[0].as_str())
            {
                return crate::pass::typecheck_core::append_type_args(
                    replacement.clone(),
                    args,
                    meta.span,
                )
                .ok_or_else(|| {
                    BoundaryFacadeCollectionError::InvalidRetainedSubstitution {
                        module_path: module_path.to_owned(),
                        binder: segments[0].name.clone(),
                    }
                });
            }
            Ok(Type::synth_path_segments(segments.clone(), args, meta.span))
        }
        Type::Forall { param, body, meta } => {
            let mut combined = HashMap::new();
            for (name, replacement) in substitution {
                if name != &param.name {
                    combined.insert(name.clone(), replacement.clone());
                }
            }
            let mut free = std::collections::HashSet::new();
            for replacement in combined.values() {
                crate::pass::typecheck_core::collect_free_type_vars(replacement, &mut free);
            }
            let new_param = if free.contains(&param.name) {
                let mut taken = free.clone();
                crate::pass::typecheck_core::collect_free_type_vars(body, &mut taken);
                taken.insert(param.name.clone());
                let fresh = crate::pass::typecheck_core::fresh_type_var(&param.name, &taken);
                combined.insert(
                    param.name.clone(),
                    Type::synth_path(vec![fresh.clone()], Vec::new(), param.span),
                );
                crate::ast::TypeParam {
                    name: fresh,
                    span: param.span,
                    kind: param.kind.clone(),
                }
            } else {
                param.clone()
            };
            Ok(Type::Forall {
                param: new_param,
                body: Box::new(substitute_retained_type(body, &combined, module_path)?),
                meta: crate::ast::Meta::new(meta.span),
            })
        }
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn retained_selective_import(imports: &[Import], name: &str) -> Option<Vec<String>> {
    imports.iter().find_map(|usage| match &usage.kind {
        ImportKind::Selective { items, from }
            if items
                .iter()
                .filter_map(ImportItem::as_name)
                .any(|item| item == name) =>
        {
            let mut out: Vec<String> = from
                .segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect();
            out.push(name.to_owned());
            Some(out)
        }
        _ => None,
    })
}

fn retained_qualified_import(imports: &[Import], alias: &str) -> Option<Vec<String>> {
    imports.iter().find_map(|usage| match &usage.kind {
        ImportKind::Qualified { path, alias: bound } if bound == alias => Some(
            path.segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect(),
        ),
        _ => None,
    })
}

fn qualified_name_from_segments(segments: &[String]) -> Option<QualifiedName> {
    let (leaf, module) = segments.split_last()?;
    if leaf.is_empty() || module.is_empty() || module.iter().any(String::is_empty) {
        return None;
    }
    Some(QualifiedName::new(module.join("/"), leaf.clone()))
}

fn retained_nominal_type(
    name: &QualifiedName,
    args: Vec<Type<Routed>>,
    span: Span,
) -> Type<Routed> {
    let mut segments: Vec<_> = name
        .module_path
        .split('/')
        .map(|segment| crate::ast::PathSegment::synth(segment, span))
        .collect();
    segments.push(crate::ast::PathSegment::synth(&name.leaf, span));
    Type::synth_path_segments(segments, args, span)
}

fn assert_stage_alignment(plan: &BoundaryCallablePlan, execution: &CallableExecutionLayout) {
    assert_stage_alignment_with(plan, execution.head_stages());
}

fn assert_stage_alignment_with(plan: &BoundaryCallablePlan, execution: &[CallableExecutionStage]) {
    let semantic = plan.entry().head_stages;
    assert_eq!(semantic.len(), execution.len());
    for (semantic, execution) in semantic.into_iter().zip(execution) {
        match (semantic, execution) {
            (
                BoundaryCallableHeadStage::Type { .. },
                CallableExecutionStage::Type {
                    action: CallableTypeStageAction::InvokeNullary,
                },
            ) => {}
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                assert_eq!(slots.len(), layout.facade_slot_count());
            }
            _ => unreachable!("paired callable stages retain the same semantic kind"),
        }
    }
}

#[cfg(test)]
pub(super) fn prepare_exported_boundary_callable(
    package: &Package<Routed>,
    module_path: &str,
    name: &str,
) -> Result<AuthoritativeBoundaryCallable, BoundaryFacadeCollectionError> {
    #[cfg(test)]
    record_live_boundary_collection_work(|work| {
        work.by_name_callable_resolutions += 1;
    });
    let entry = package
        .module(module_path)
        .ok_or_else(|| missing_declaration(module_path, format!("exported function {name:?}")))?;
    let declaration = entry
        .module
        .items
        .iter()
        .find_map(|item| match item {
            Item::FnDef(declaration)
                if declaration.name == name && declaration.vis.is_exported() =>
            {
                Some(declaration)
            }
            _ => None,
        })
        .ok_or_else(|| missing_declaration(module_path, format!("exported function {name:?}")))?;
    Ok(prepare_exported_boundary_callable_from_declaration(
        package,
        entry,
        declaration,
    ))
}

fn prepare_exported_boundary_callable_from_declaration(
    package: &Package<Routed>,
    entry: &crate::pass::resolve::ModuleEntry<Routed>,
    declaration: &crate::ast::FnDef<Routed>,
) -> AuthoritativeBoundaryCallable {
    #[cfg(test)]
    record_live_boundary_collection_work(|work| {
        work.direct_callable_preparations += 1;
    });
    debug_assert!(declaration.vis.is_exported());
    let site = site_id(
        &entry.module,
        BoundaryFacadeSiteOwner::ExportedFunction {
            name: declaration.name.clone(),
        },
    );
    let (signature, ret) = crate::pass::resolve::exported_routed_contract_fn_signature_and_ret(
        &declaration.sig,
        &declaration.ret,
        entry,
    );
    AuthoritativeBoundaryCallable::from_signature(site, &signature, &ret, package)
}

#[cfg(test)]
pub(super) fn prepare_host_boundary_callable(
    package: &Package<Routed>,
    module_path: &str,
    name: &str,
) -> Result<AuthoritativeBoundaryCallable, BoundaryFacadeCollectionError> {
    #[cfg(test)]
    record_live_boundary_collection_work(|work| {
        work.by_name_callable_resolutions += 1;
    });
    let entry = package
        .module(module_path)
        .ok_or_else(|| missing_declaration(module_path, format!("host function {name:?}")))?;
    let declaration = entry
        .module
        .items
        .iter()
        .find_map(|item| match item {
            Item::HostFn(declaration) if declaration.name == name => Some(declaration),
            _ => None,
        })
        .ok_or_else(|| missing_declaration(module_path, format!("host function {name:?}")))?;
    Ok(prepare_host_boundary_callable_from_declaration(
        package,
        entry,
        declaration,
    ))
}

fn prepare_host_boundary_callable_from_declaration(
    package: &Package<Routed>,
    entry: &crate::pass::resolve::ModuleEntry<Routed>,
    declaration: &crate::ast::HostFn<Routed>,
) -> AuthoritativeBoundaryCallable {
    #[cfg(test)]
    record_live_boundary_collection_work(|work| {
        work.direct_callable_preparations += 1;
    });
    let site = site_id(
        &entry.module,
        BoundaryFacadeSiteOwner::HostFunction {
            name: declaration.name.clone(),
        },
    );
    let (signature, ret) = host_function_signature(&entry.module, declaration);
    AuthoritativeBoundaryCallable::from_signature(site, &signature, &ret, package)
}

#[cfg(test)]
pub(super) fn prepare_newtype_constructor_boundary_callable(
    package: &Package<Routed>,
    module_path: &str,
    newtype: &str,
) -> Result<AuthoritativeBoundaryCallable, BoundaryFacadeCollectionError> {
    prepare_newtype_member_boundary_callable(
        package,
        module_path,
        newtype,
        NewtypeMemberRole::Constructor,
    )
}

#[cfg(test)]
pub(super) fn prepare_newtype_projector_boundary_callable(
    package: &Package<Routed>,
    module_path: &str,
    newtype: &str,
) -> Result<AuthoritativeBoundaryCallable, BoundaryFacadeCollectionError> {
    prepare_newtype_member_boundary_callable(
        package,
        module_path,
        newtype,
        NewtypeMemberRole::Projector,
    )
}

#[cfg(test)]
fn prepare_newtype_member_boundary_callable(
    package: &Package<Routed>,
    module_path: &str,
    newtype: &str,
    role: NewtypeMemberRole,
) -> Result<AuthoritativeBoundaryCallable, BoundaryFacadeCollectionError> {
    #[cfg(test)]
    record_live_boundary_collection_work(|work| {
        work.by_name_callable_resolutions += 1;
    });
    let entry = package
        .module(module_path)
        .ok_or_else(|| missing_declaration(module_path, format!("newtype {newtype:?}")))?;
    let declaration = entry
        .module
        .items
        .iter()
        .find_map(|item| {
            let mut found = None;
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(declaration) = declaration.newtype()
                    && declaration.name == newtype
                    && declaration.vis.is_exported()
                {
                    found = Some(declaration);
                }
            });
            found
        })
        .ok_or_else(|| missing_declaration(module_path, format!("newtype {newtype:?}")))?;
    prepare_newtype_member_boundary_callable_from_declaration(
        package,
        module_path,
        entry,
        declaration,
        role,
    )
}

fn prepare_newtype_member_boundary_callable_from_declaration(
    package: &Package<Routed>,
    module_path: &str,
    entry: &crate::pass::resolve::ModuleEntry<Routed>,
    declaration: &crate::ast::Newtype<Routed>,
    role: NewtypeMemberRole,
) -> Result<AuthoritativeBoundaryCallable, BoundaryFacadeCollectionError> {
    #[cfg(test)]
    record_live_boundary_collection_work(|work| {
        work.direct_callable_preparations += 1;
    });
    debug_assert!(declaration.vis.is_exported());
    let member = match role {
        NewtypeMemberRole::Constructor => &declaration.constructor,
        NewtypeMemberRole::Projector => &declaration.projector,
    };
    if !member.vis.is_exported() {
        return Err(missing_declaration(
            module_path,
            format!("newtype member {:?}.{:?}", declaration.name, member.name),
        ));
    }
    let owner = match role {
        NewtypeMemberRole::Constructor => BoundaryFacadeSiteOwner::NewtypeConstructor {
            newtype: declaration.name.clone(),
            member: member.name.clone(),
        },
        NewtypeMemberRole::Projector => BoundaryFacadeSiteOwner::NewtypeProjector {
            newtype: declaration.name.clone(),
            member: member.name.clone(),
        },
    };
    let site = site_id(&entry.module, owner);
    let mut nominal_segments = entry.module.path.segments.clone();
    nominal_segments.push(crate::ast::PathSegment::synth(
        declaration.name.clone(),
        declaration.meta.span,
    ));
    let scheme = crate::pass::typecheck_core::newtype_member_scheme_in_module(
        declaration,
        &member.name,
        &nominal_segments,
        &entry.module,
        Some(package),
        declaration.meta.span,
    )
    .unwrap_or_else(|error| {
        unreachable!("an exact newtype member must synthesize its scheme: {error:?}")
    });
    Ok(AuthoritativeBoundaryCallable::from_scheme(
        site,
        scheme.ty.as_type().clone(),
        newtype_member_source_head_stages(scheme.ty.as_type(), declaration, role),
        package,
    ))
}

#[derive(Clone, Copy)]
enum NewtypeMemberRole {
    Constructor,
    Projector,
}

fn missing_declaration(module_path: &str, selector: String) -> BoundaryFacadeCollectionError {
    BoundaryFacadeCollectionError::MissingDeclaration {
        module_path: module_path.to_owned(),
        selector,
    }
}

fn site_id(
    module: &crate::ast::Module<Routed>,
    owner: BoundaryFacadeSiteOwner,
) -> BoundaryFacadeSiteId {
    BoundaryFacadeSiteId::new(
        module
            .path
            .segments
            .iter()
            .map(|segment| segment.name.clone())
            .collect(),
        owner,
    )
    .expect("a resolved module and declaration have non-empty identity components")
}

fn host_function_signature(
    module: &crate::ast::Module<Routed>,
    declaration: &crate::ast::HostFn<Routed>,
) -> (Signature<Routed>, Type<Routed>) {
    let mut binder_kinds = std::collections::HashMap::new();
    let params = declaration
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| match param {
            crate::ast::HostFnParam::Type(param) => {
                binder_kinds.insert(param.name.clone(), param.effective_kind());
                crate::ast::SignatureParam::Type(param.clone())
            }
            crate::ast::HostFnParam::Value(param) => {
                crate::ast::SignatureParam::Value(crate::ast::Param {
                    name: param.name.clone().unwrap_or_else(|| format!("_p{index}")),
                    ty: Some(
                        crate::pass::resolve::qualify_routed_contract_type_in_module(
                            &param.ty,
                            module,
                            &binder_kinds,
                        ),
                    ),
                    pattern: (),
                    meta: param.meta.clone(),
                })
            }
        })
        .collect();
    let ret = crate::pass::resolve::qualify_routed_contract_type_in_module(
        &declaration.ret,
        module,
        &binder_kinds,
    );
    (
        Signature::from_parts(params, declaration.param_groups.clone()),
        ret,
    )
}

fn canonicalize_authoritative_scheme(
    scheme: &Type<Routed>,
    package: &Package<Routed>,
    binder_locals: Option<&std::collections::HashSet<String>>,
) -> Type<Routed> {
    let local = std::collections::HashMap::new();
    let cross_module = std::collections::HashMap::new();
    let aliases = crate::pass::typecheck_core::AliasCtx {
        local: &local,
        cross_module: &cross_module,
        type_interner: None,
        source_module: None,
        package: Some(package),
        binder_locals,
    };
    let (canonical, identity_canonical) =
        crate::pass::typecheck_core::canonicalize_deep_for_comparison(scheme, &aliases, true);
    if !identity_canonical {
        unreachable!("an authoritative boundary scheme has canonical nominal identity");
    }
    canonical
}

fn signature_head_stage_kinds(signature: &Signature<Routed>) -> Vec<BoundaryCallableHeadStageKind> {
    let mut head_stage_kinds = Vec::new();
    let mut has_value_group = false;
    for group in signature.canonical_groups() {
        match group {
            SignatureGroupRef::Type(params) => {
                head_stage_kinds.extend(params.iter().map(|_| BoundaryCallableHeadStageKind::Type))
            }
            SignatureGroupRef::Value(_) => {
                has_value_group = true;
                head_stage_kinds.push(BoundaryCallableHeadStageKind::Value);
            }
        }
    }
    if !has_value_group {
        head_stage_kinds.push(BoundaryCallableHeadStageKind::Value);
    }
    head_stage_kinds
}

fn signature_source_head_stages(
    signature: &Signature<Routed>,
) -> Vec<AuthoritativeBoundaryHeadStage> {
    let mut stages = Vec::new();
    let mut has_value_group = false;
    for group in signature.canonical_groups() {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params {
                    let crate::ast::SignatureParam::Type(param) = param else {
                        unreachable!("a canonical type group contains only type parameters");
                    };
                    stages.push(AuthoritativeBoundaryHeadStage::Type {
                        param: param.clone(),
                    });
                }
            }
            SignatureGroupRef::Value(params) => {
                has_value_group = true;
                stages.push(AuthoritativeBoundaryHeadStage::Value {
                    source_params: params
                        .iter()
                        .map(|param| {
                            let crate::ast::SignatureParam::Value(param) = param else {
                                unreachable!(
                                    "a canonical value group contains only value parameters"
                                );
                            };
                            param
                                .ty
                                .clone()
                                .expect("a declaration-owned value parameter has an exact type")
                        })
                        .collect(),
                });
            }
        }
    }
    if !has_value_group {
        stages.push(AuthoritativeBoundaryHeadStage::Value {
            source_params: Vec::new(),
        });
    }
    stages
}

fn source_head_stage_kinds(
    stages: &[AuthoritativeBoundaryHeadStage],
) -> Vec<BoundaryCallableHeadStageKind> {
    stages
        .iter()
        .map(|stage| match stage {
            AuthoritativeBoundaryHeadStage::Type { .. } => BoundaryCallableHeadStageKind::Type,
            AuthoritativeBoundaryHeadStage::Value { .. } => BoundaryCallableHeadStageKind::Value,
        })
        .collect()
}

fn newtype_member_source_head_stages(
    scheme: &Type<Routed>,
    declaration: &crate::ast::Newtype<Routed>,
    role: NewtypeMemberRole,
) -> Vec<AuthoritativeBoundaryHeadStage> {
    let type_stage_count = declaration.type_params.len()
        + match role {
            NewtypeMemberRole::Constructor => declaration.existential_params.len(),
            NewtypeMemberRole::Projector => 0,
        };
    let mut current = scheme;
    let mut stages = Vec::with_capacity(type_stage_count + 1);
    for _ in 0..type_stage_count {
        let Type::Forall { param, body, .. } = current else {
            unreachable!("a newtype member scheme retains each owner-derived type stage");
        };
        stages.push(AuthoritativeBoundaryHeadStage::Type {
            param: param.clone(),
        });
        current = body;
    }
    let Type::Function { param, .. } = current else {
        unreachable!("a newtype member scheme retains its owner-derived value stage");
    };
    stages.push(AuthoritativeBoundaryHeadStage::Value {
        source_params: vec![param.as_ref().clone()],
    });
    stages
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use super::*;
    use crate::ast::{
        BridgeBlock, BridgeGlob, BridgeGlobSegment, Expr, FnDef, HostFn, HostFnParam,
        HostFnValueParam, HostType, Kind, Lifetime, Meta, Module, ModulePath, Newtype, PackageFile,
        Param, PathSegment, Prime, RoleAnnotation, SignatureGroup, TypeAlias, TypeMember,
        TypeParam, Visibility,
    };
    use crate::pass::resolve::{ModuleEntry, PackageFileEntry, TopLevelScope};

    fn span() -> Span {
        Span::new(0, 0)
    }

    fn meta() -> Meta<Routed> {
        Meta::new(span())
    }

    fn bare(name: &str) -> Type<Routed> {
        bare_args(name, Vec::new())
    }

    fn bare_args(name: &str, args: Vec<Type<Routed>>) -> Type<Routed> {
        Type::Path {
            segments: vec![PathSegment::new(name, span())],
            args,
            meta: meta(),
        }
    }

    fn nominal(module: &str, name: &str) -> Type<Routed> {
        nominal_args(module, name, Vec::new())
    }

    fn nominal_args(module: &str, name: &str, args: Vec<Type<Routed>>) -> Type<Routed> {
        Type::Path {
            segments: module
                .split('/')
                .chain(std::iter::once(name))
                .map(|part| PathSegment::new(part, span()))
                .collect(),
            args,
            meta: meta(),
        }
    }

    fn unit() -> Type<Routed> {
        Type::Unit { meta: meta() }
    }

    fn product(left: Type<Routed>, right: Type<Routed>) -> Type<Routed> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: meta(),
        }
    }

    fn sum(left: Type<Routed>, right: Type<Routed>) -> Type<Routed> {
        Type::Sum {
            left: Box::new(left),
            right: Box::new(right),
            meta: meta(),
        }
    }

    fn function(param: Type<Routed>, ret: Type<Routed>, abi_arity: usize) -> Type<Routed> {
        Type::Function {
            param: Box::new(param),
            ret: Box::new(ret),
            meta: meta(),
            abi_arity,
            caps: FnTypeCapabilities::default(),
        }
    }

    fn forall(name: &str, body: Type<Routed>) -> Type<Routed> {
        forall_kind(name, Kind::Star, body)
    }

    fn forall_kind(name: &str, kind: Kind, body: Type<Routed>) -> Type<Routed> {
        Type::Forall {
            param: TypeParam {
                name: name.to_owned(),
                span: span(),
                kind: Some(kind),
            },
            body: Box::new(body),
            meta: meta(),
        }
    }

    fn prepared<'a>(
        ty: &'a Type<Routed>,
        semantic: &'a BTreeSet<QualifiedTypeName>,
    ) -> PreparedBoundaryScheme<'a> {
        PreparedBoundaryScheme::try_new(ty, semantic).expect("prepared test scheme")
    }

    fn plan(ty: &Type<Routed>) -> BoundaryFacadePlan {
        BoundaryFacadePlan::from_prepared_scheme(prepared(ty, &BTreeSet::new()))
    }

    fn callable_plan_with_execution(
        ty: &Type<Routed>,
    ) -> (BoundaryCallablePlan, BoundaryFacadeExecutionPlan) {
        BoundaryCallablePlan::from_authoritative_scheme_with_execution(
            ty,
            Vec::new(),
            &BTreeSet::new(),
        )
        .expect("prepared execution test scheme")
    }

    fn semantic_name(module: &str, name: &str) -> QualifiedTypeName {
        QualifiedTypeName::new(
            module.split('/').map(str::to_owned).collect(),
            name.to_owned(),
        )
        .expect("qualified semantic name")
    }

    fn expect_forall(plan: &BoundaryFacadePlan, id: FacadeUseId) -> (FacadeBinderId, FacadeUseId) {
        let FacadeUse::Forall { binder, result, .. } = plan.use_at(id) else {
            panic!("expected Forall, got {:?}", plan.use_at(id));
        };
        (*binder, *result)
    }

    fn expect_function(
        plan: &BoundaryFacadePlan,
        id: FacadeUseId,
    ) -> (&[FacadeUseId], FacadeUseId) {
        let FacadeUse::Function { slots, result, .. } = plan.use_at(id) else {
            panic!("expected Function, got {:?}", plan.use_at(id));
        };
        (slots, *result)
    }

    fn expect_product(
        plan: &BoundaryFacadePlan,
        id: FacadeUseId,
    ) -> (&FacadeShellId, &[FacadeUseId]) {
        let FacadeUse::Product { shell, args, .. } = plan.use_at(id) else {
            panic!("expected Product, got {:?}", plan.use_at(id));
        };
        (shell, args)
    }

    fn expect_execution_function(
        execution: &BoundaryFacadeExecutionPlan,
        id: FacadeUseId,
    ) -> &CallableValueStageLayout {
        let BoundaryFacadeExecutionUse::Function(layout) = execution.use_at(id) else {
            panic!("expected function execution at {id:?}")
        };
        layout
    }

    #[test]
    fn scheme_first_stages_ignore_non_unit_abi_arity_and_canonical_unit_has_no_slots() {
        let raw = forall(
            "T",
            function(
                product(
                    nominal("api", "A"),
                    product(nominal("api", "B"), nominal("api", "C")),
                ),
                function(unit(), nominal("api", "R"), 0),
                0,
            ),
        );
        let different_private_layout = forall(
            "T",
            function(
                product(
                    nominal("api", "A"),
                    product(nominal("api", "B"), nominal("api", "C")),
                ),
                function(unit(), nominal("api", "R"), 0),
                3,
            ),
        );
        assert_eq!(plan(&raw), plan(&different_private_layout));

        let plan = plan(&raw);
        let (_, outer_id) = expect_forall(&plan, plan.root());
        let (outer, inner_id) = expect_function(&plan, outer_id);
        let (inner, result) = expect_function(&plan, inner_id);

        assert_eq!(outer.len(), 3);
        assert!(inner.is_empty());
        assert!(matches!(
            plan.use_at(result),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "R")
        ));
    }

    #[test]
    fn execution_plan_is_total_for_returned_nested_functions_and_foralls() {
        let callback = function(
            nominal("host", "Input"),
            forall(
                "CallbackResult",
                function(bare("CallbackResult"), nominal("host", "CallbackDone"), 1),
            ),
            1,
        );
        let ty = function(
            callback,
            forall(
                "Returned",
                function(
                    bare("Returned"),
                    function(nominal("host", "Tail"), nominal("host", "Done"), 1),
                    1,
                ),
            ),
            1,
        );
        let (callable, execution) = callable_plan_with_execution(&ty);
        let facade = callable.facade();
        execution
            .validate_alignment(facade)
            .expect("total nested execution alignment");
        assert_eq!(execution.uses().len(), facade.uses().len());

        let (root_slots, returned_forall) = expect_function(facade, facade.root());
        let (callback_slots, callback_forall) = expect_function(facade, root_slots[0]);
        assert_eq!(callback_slots.len(), 1);
        let (_, callback_function) = expect_forall(facade, callback_forall);
        assert_eq!(expect_function(facade, callback_function).0.len(), 1);
        let (_, returned_function) = expect_forall(facade, returned_forall);
        let (_, returned_callback) = expect_function(facade, returned_function);
        assert_eq!(expect_function(facade, returned_callback).0.len(), 1);

        for (index, semantic) in facade.uses().iter().enumerate() {
            let id = FacadeUseId(index);
            match semantic {
                FacadeUse::Function { .. } => {
                    assert!(matches!(
                        execution.use_at(id),
                        BoundaryFacadeExecutionUse::Function(_)
                    ));
                }
                FacadeUse::Forall { .. } => {
                    assert!(matches!(
                        execution.use_at(id),
                        BoundaryFacadeExecutionUse::InvokeForall
                    ));
                }
                _ => assert!(matches!(
                    execution.use_at(id),
                    BoundaryFacadeExecutionUse::NoAction
                )),
            }
        }
    }

    #[test]
    fn per_use_execution_distinguishes_abi_nullary_from_substituted_unit() {
        let abi_nullary = function(unit(), nominal("host", "Result"), 0);
        let substituted_unit = function(unit(), nominal("host", "Result"), 1);
        let (nullary_plan, nullary_execution) = callable_plan_with_execution(&abi_nullary);
        let (unit_plan, unit_execution) = callable_plan_with_execution(&substituted_unit);

        assert_ne!(nullary_plan, unit_plan);
        assert_ne!(nullary_execution, unit_execution);
        let nullary = expect_execution_function(&nullary_execution, nullary_plan.facade().root());
        assert_eq!(nullary.source_param_count(), 0);
        assert_eq!(nullary.body_abi_arity(), 0);
        assert!(nullary.source_params().is_empty());
        let (unit_slots, _) = expect_function(unit_plan.facade(), unit_plan.facade().root());
        assert_eq!(unit_slots.len(), 1);
        assert!(matches!(
            unit_plan.facade().use_at(unit_slots[0]),
            FacadeUse::Unit { .. }
        ));
        let unit = expect_execution_function(&unit_execution, unit_plan.facade().root());
        assert_eq!(unit.source_param_count(), 1);
        assert_eq!(unit.body_abi_arity(), 1);
        let [unit_param] = unit.source_params() else {
            panic!("substituted Unit owns one source range")
        };
        assert_eq!(unit_param.facade_slots(), 0..1);
        assert_eq!(unit_param.adapter(), CallableSourceParamAdapter::Identity);

        let mut misaligned = unit_execution.clone();
        misaligned.uses[unit_plan.facade().root().0] = BoundaryFacadeExecutionUse::NoAction;
        assert_eq!(
            misaligned.validate_alignment(unit_plan.facade()),
            Err("one execution entry disagrees with its semantic facade use")
        );

        let no_declaration_params = Vec::<TypeParam>::new();
        let (nullary_payload, nullary_payload_execution) =
            prepare_newtype_payload_plan_with_presentation(
                abi_nullary,
                no_declaration_params.iter(),
                &BTreeSet::new(),
            )
            .expect("ABI-nullary semantic payload");
        let (unit_payload, unit_payload_execution) =
            prepare_newtype_payload_plan_with_presentation(
                substituted_unit,
                no_declaration_params.iter(),
                &BTreeSet::new(),
            )
            .expect("substituted-Unit semantic payload");
        assert_ne!(nullary_payload, unit_payload);
        assert_ne!(nullary_payload_execution, unit_payload_execution);
    }

    #[test]
    fn substituted_product_stays_one_execution_slot_while_direct_product_expands() {
        let product_ty = product(nominal("host", "Left"), nominal("host", "Right"));
        let direct_ty = function(product_ty.clone(), nominal("host", "Result"), 1);
        let (direct_plan, direct_execution) = callable_plan_with_execution(&direct_ty);
        let direct = expect_execution_function(&direct_execution, direct_plan.facade().root());
        let [direct_param] = direct.source_params() else {
            panic!("one direct product source parameter")
        };
        assert_eq!(direct.facade_slot_count(), 2);
        assert_eq!(direct_param.facade_slots(), 0..2);
        assert_eq!(
            direct_param.adapter(),
            CallableSourceParamAdapter::RightNest
        );

        let generic_ty = forall("T", function(bare("T"), nominal("host", "Result"), 1));
        let (generic_plan, generic_execution) = callable_plan_with_execution(&generic_ty);
        assert!(matches!(
            generic_execution.use_at(generic_plan.facade().root()),
            BoundaryFacadeExecutionUse::InvokeForall
        ));
        let (_, generic_function) =
            expect_forall(generic_plan.facade(), generic_plan.facade().root());
        let generic = expect_execution_function(&generic_execution, generic_function);
        let [generic_param] = generic.source_params() else {
            panic!("one generic source parameter")
        };
        assert_eq!(generic.facade_slot_count(), 1);
        assert_eq!(generic_param.facade_slots(), 0..1);
        assert_eq!(
            generic_param.adapter(),
            CallableSourceParamAdapter::Identity
        );

        let applied = generic_plan
            .facade()
            .apply_leading_type_arg(&plan(&product_ty))
            .expect("leading generic application");
        let (slots, _) = expect_function(&applied, applied.root());
        assert_eq!(slots.len(), 1);
        assert!(matches!(
            applied.use_at(slots[0]),
            FacadeUse::Product { args, .. } if args.len() == 2
        ));
        assert_eq!(generic_param.facade_slots(), 0..1);
    }

    #[test]
    fn named_unit_and_direct_callable_each_remain_one_raw_slot() {
        let callback = function(
            product(nominal("api", "A"), nominal("api", "B")),
            nominal("api", "C"),
            1,
        );
        let raw = function(
            product(nominal("api", "Unit"), callback),
            nominal("api", "R"),
            99,
        );
        let plan = plan(&raw);
        let (slots, _) = expect_function(&plan, plan.root());
        assert_eq!(slots.len(), 2);
        assert!(matches!(plan.use_at(slots[0]), FacadeUse::Nominal { .. }));
        assert_eq!(expect_function(&plan, slots[1]).0.len(), 2);
    }

    #[test]
    fn semantic_keys_follow_bare_qualified_positional_fallback() {
        let ty = product(
            nominal("m1", "Foo"),
            product(
                nominal("m2", "Foo"),
                product(
                    nominal("m1", "Foo"),
                    product(nominal("m1", "Foo"), nominal("host", "Scalar")),
                ),
            ),
        );
        let semantic = BTreeSet::from([semantic_name("m1", "Foo"), semantic_name("m2", "Foo")]);
        let plan = BoundaryFacadePlan::from_prepared_scheme(prepared(&ty, &semantic));
        let (shell, _) = expect_product(&plan, plan.root());

        assert_eq!(
            shell.ordered_keys(),
            &[
                SemanticKey::Bare {
                    name: "Foo".to_owned(),
                },
                SemanticKey::Qualified {
                    module_segments: vec!["m2".to_owned()],
                    name: "Foo".to_owned(),
                },
                SemanticKey::Qualified {
                    module_segments: vec!["m1".to_owned()],
                    name: "Foo".to_owned(),
                },
                SemanticKey::Positional { index: 3 },
                SemanticKey::Positional { index: 4 },
            ]
        );
    }

    #[test]
    fn applied_nominal_keys_agree_across_value_and_callable_shells() {
        let slots = product(
            nominal_args("m1", "Foo", vec![unit()]),
            product(
                nominal_args("m2", "Foo", vec![unit()]),
                product(
                    nominal_args("m1", "Foo", vec![nominal("host", "Scalar")]),
                    product(
                        nominal_args("m1", "Foo", vec![unit()]),
                        product(
                            nominal_args("host", "Box", vec![nominal("m1", "Foo")]),
                            bare_args("F", vec![nominal("m1", "Foo")]),
                        ),
                    ),
                ),
            ),
        );
        let scheme = forall_kind("F", Kind::arrow_chain(1), function(slots.clone(), slots, 1));
        let semantic = BTreeSet::from([semantic_name("m1", "Foo"), semantic_name("m2", "Foo")]);
        let (callable, execution) = BoundaryCallablePlan::from_authoritative_scheme_with_execution(
            &scheme,
            vec![
                BoundaryCallableHeadStageKind::Type,
                BoundaryCallableHeadStageKind::Value,
            ],
            &semantic,
        )
        .expect("generic nominal boundary");
        let plan = callable.facade();
        let (_, function_id) = expect_forall(plan, plan.root());
        let (_, result_id) = expect_function(plan, function_id);
        let (result_shell, _) = expect_product(plan, result_id);
        assert_eq!(
            result_shell.ordered_keys(),
            &[
                SemanticKey::Bare {
                    name: "Foo".to_owned()
                },
                SemanticKey::Qualified {
                    module_segments: vec!["m2".to_owned()],
                    name: "Foo".to_owned()
                },
                SemanticKey::Qualified {
                    module_segments: vec!["m1".to_owned()],
                    name: "Foo".to_owned()
                },
                SemanticKey::Positional { index: 3 },
                SemanticKey::Positional { index: 4 },
                SemanticKey::Positional { index: 5 },
            ]
        );
        assert_eq!(plan.function_shell(function_id), Some(result_shell));
        assert_eq!(callable.head_value_shell(), Some(result_shell));
        let layout = expect_execution_function(&execution, function_id);
        assert_eq!(layout.facade_shell.as_ref(), Some(result_shell));
        let [source] = layout.source_params() else {
            panic!("one product source parameter")
        };
        assert_eq!(source.adapter(), CallableSourceParamAdapter::RightNest);
        assert_eq!(source.product_shell(), Some(result_shell));
    }

    #[test]
    fn shell_identity_is_generic_over_payloads_and_nested_uses_keep_grouping() {
        let right_flat = product(
            nominal("api", "A"),
            product(nominal("api", "B"), nominal("api", "C")),
        );
        let other_payloads = product(
            nominal("api", "X"),
            product(nominal("api", "Y"), nominal("api", "Z")),
        );
        let left_nested = product(
            product(nominal("api", "A"), nominal("api", "B")),
            nominal("api", "C"),
        );
        let flat_plan = plan(&right_flat);
        let other_plan = plan(&other_payloads);
        let nested_plan = plan(&left_nested);
        let (flat_shell, flat_args) = expect_product(&flat_plan, flat_plan.root());
        let (other_shell, _) = expect_product(&other_plan, other_plan.root());
        let (nested_shell, nested_args) = expect_product(&nested_plan, nested_plan.root());

        assert_eq!(flat_shell, other_shell);
        assert_eq!(flat_args.len(), 3);
        assert_eq!(nested_args.len(), 2);
        assert_eq!(nested_shell.ordered_keys().len(), 2);
        assert!(matches!(
            nested_plan.use_at(nested_args[0]),
            FacadeUse::Product { args, .. } if args.len() == 2
        ));

        let sum_right = sum(
            nominal("api", "A"),
            sum(nominal("api", "B"), nominal("api", "C")),
        );
        let sum_left = sum(
            sum(nominal("api", "A"), nominal("api", "B")),
            nominal("api", "C"),
        );
        let right_plan = plan(&sum_right);
        let left_plan = plan(&sum_left);
        assert!(matches!(
            right_plan.use_at(right_plan.root()),
            FacadeUse::Sum { args, .. } if args.len() == 3
        ));
        assert!(matches!(
            left_plan.use_at(left_plan.root()),
            FacadeUse::Sum { args, .. }
                if args.len() == 2
                    && matches!(left_plan.use_at(args[0]), FacadeUse::Sum { .. })
        ));
    }

    #[test]
    fn substituting_unit_product_or_function_keeps_one_parent_slot() {
        let raw = forall("T", function(bare("T"), nominal("api", "R"), 7));
        let parent = plan(&raw);

        let unit_applied = parent
            .apply_leading_type_arg(&plan(&unit()))
            .expect("leading type stage");
        let (unit_slots, _) = expect_function(&unit_applied, unit_applied.root());
        assert_eq!(unit_slots.len(), 1);
        assert!(matches!(
            unit_applied.use_at(unit_slots[0]),
            FacadeUse::Unit { .. }
        ));

        let product_arg = plan(&product(nominal("api", "A"), nominal("api", "B")));
        let product_applied = parent
            .apply_leading_type_arg(&product_arg)
            .expect("leading type stage");
        let (product_slots, _) = expect_function(&product_applied, product_applied.root());
        assert_eq!(product_slots.len(), 1);
        assert!(matches!(
            product_applied.use_at(product_slots[0]),
            FacadeUse::Product { args, .. } if args.len() == 2
        ));

        let function_arg = plan(&function(
            product(nominal("api", "A"), nominal("api", "B")),
            nominal("api", "C"),
            1,
        ));
        let function_applied = parent
            .apply_leading_type_arg(&function_arg)
            .expect("leading type stage");
        let (function_slots, _) = expect_function(&function_applied, function_applied.root());
        assert_eq!(function_slots.len(), 1);
        assert_eq!(
            expect_function(&function_applied, function_slots[0])
                .0
                .len(),
            2
        );
    }

    #[test]
    fn higher_kinded_substitution_preserves_application_nodes() {
        let raw = forall_kind(
            "F",
            Kind::arrow_chain(1),
            forall(
                "A",
                function(
                    Type::Path {
                        segments: vec![PathSegment::new("F", span())],
                        args: vec![bare("A")],
                        meta: meta(),
                    },
                    Type::Path {
                        segments: vec![PathSegment::new("F", span())],
                        args: vec![bare("A")],
                        meta: meta(),
                    },
                    1,
                ),
            ),
        );
        let constructor = plan(&nominal("host", "Box"));
        let item = plan(&nominal("host", "Item"));
        let applied_constructor = plan(&raw)
            .apply_leading_type_arg(&constructor)
            .expect("constructor binder");
        let applied = applied_constructor
            .apply_leading_type_arg(&item)
            .expect("item binder");
        let (slots, result) = expect_function(&applied, applied.root());

        for use_id in [slots[0], result] {
            let FacadeUse::Apply {
                constructor, args, ..
            } = applied.use_at(use_id)
            else {
                panic!("expected applied nominal");
            };
            assert_eq!(args.len(), 1);
            assert!(matches!(
                applied.use_at(*constructor),
                FacadeUse::Nominal { name, .. }
                    if name == &semantic_name("host", "Box")
            ));
            assert!(matches!(
                applied.use_at(args[0]),
                FacadeUse::Nominal { name, .. }
                    if name == &semantic_name("host", "Item")
            ));
        }
    }

    #[test]
    fn binder_ids_make_shadowing_and_substitution_capture_free() {
        let raw = forall(
            "T",
            forall("T", function(bare("T"), nominal("api", "R"), 1)),
        );
        let raw_plan = plan(&raw);
        let (outer, after_outer) = expect_forall(&raw_plan, raw_plan.root());
        let (inner, after_inner) = expect_forall(&raw_plan, after_outer);
        let (slots, _) = expect_function(&raw_plan, after_inner);
        assert_ne!(outer, inner);
        assert!(matches!(
            raw_plan.use_at(slots[0]),
            FacadeUse::Bound { binder, .. } if binder == &inner
        ));

        let replacement = plan(&nominal("host", "Replacement"));
        let applied = raw_plan
            .apply_leading_type_arg(&replacement)
            .expect("outer type stage");
        let (remaining, body) = expect_forall(&applied, applied.root());
        let (slots, _) = expect_function(&applied, body);
        assert!(matches!(
            applied.use_at(slots[0]),
            FacadeUse::Bound { binder, .. } if binder == &remaining
        ));
    }

    #[test]
    fn prepared_boundary_rejects_nominal_guesses_and_stops_at_qualified_paths() {
        let semantic = BTreeSet::new();
        assert_eq!(
            PreparedBoundaryScheme::try_new(&bare("Foo"), &semantic)
                .expect_err("bare nominal must be rejected"),
            PreparedSchemeError::UnqualifiedNominal("Foo".to_owned())
        );

        let recursive = forall(
            "T",
            function(
                nominal_args("tree", "Node", vec![bare("T")]),
                nominal_args("tree", "Node", vec![bare("T")]),
                1,
            ),
        );
        let plan = BoundaryFacadePlan::from_prepared_scheme(
            PreparedBoundaryScheme::try_new(&recursive, &semantic)
                .expect("qualified recursive nominal"),
        );
        let (_, body) = expect_forall(&plan, plan.root());
        let (slots, result) = expect_function(&plan, body);
        for use_id in [slots[0], result] {
            let FacadeUse::Apply { constructor, .. } = plan.use_at(use_id) else {
                panic!("recursive nominal remains an application");
            };
            assert!(matches!(
                plan.use_at(*constructor),
                FacadeUse::Nominal { name, .. }
                    if name == &semantic_name("tree", "Node")
            ));
        }
        assert!(plan.uses().len() < 16);
    }

    fn value_param(name: &str, ty: Type<Routed>) -> Param<Routed> {
        Param {
            name: name.to_owned(),
            ty: Some(ty),
            pattern: (),
            meta: meta(),
        }
    }

    #[test]
    fn callable_cut_separates_declaration_head_from_callable_return() {
        let signature = Signature::from_groups(vec![
            SignatureGroup::Value(vec![value_param("x", nominal("api", "A"))]),
            SignatureGroup::Type(vec![TypeParam {
                name: "T".to_owned(),
                span: span(),
                kind: None,
            }]),
            SignatureGroup::Value(vec![value_param("y", bare("T"))]),
        ]);
        let ret = function(nominal("api", "C"), nominal("api", "R"), 1);
        let callable =
            BoundaryCallablePlan::from_prepared_signature(&signature, &ret, &BTreeSet::new())
                .expect("prepared callable");
        let entry = callable.entry();

        assert_eq!(
            callable.head_stage_kinds(),
            &[
                BoundaryCallableHeadStageKind::Value,
                BoundaryCallableHeadStageKind::Type,
                BoundaryCallableHeadStageKind::Value,
            ]
        );
        assert!(matches!(
            entry.head_stages[0],
            BoundaryCallableHeadStage::Value { slots } if slots.len() == 1
        ));
        let binder_id = match entry.head_stages[1] {
            BoundaryCallableHeadStage::Type { id, binder }
                if callable.facade().binder(id) == binder && binder.name == "T" =>
            {
                id
            }
            other => panic!("expected linked type stage, got {other:?}"),
        };
        let bound_slot = match entry.head_stages[2] {
            BoundaryCallableHeadStage::Value { slots } if slots.len() == 1 => slots[0],
            other => panic!("expected final value stage, got {other:?}"),
        };
        assert!(matches!(
            callable.facade().use_at(bound_slot),
            FacadeUse::Bound { binder, .. } if binder == &binder_id
        ));
        assert!(matches!(
            entry.head_stages[1],
            BoundaryCallableHeadStage::Type { .. }
        ));
        assert_eq!(
            expect_function(callable.facade(), entry.returned).0.len(),
            1
        );
    }

    #[test]
    fn callable_cut_retains_canonical_zero_slot_value_group() {
        let signature: Signature<Routed> =
            Signature::from_groups(vec![SignatureGroup::Type(vec![TypeParam {
                name: "T".to_owned(),
                span: span(),
                kind: None,
            }])]);
        let callable = BoundaryCallablePlan::from_prepared_signature(
            &signature,
            &nominal("api", "R"),
            &BTreeSet::new(),
        )
        .expect("prepared callable");
        let entry = callable.entry();

        assert_eq!(entry.head_stages.len(), 2);
        assert!(matches!(
            entry.head_stages[1],
            BoundaryCallableHeadStage::Value { slots } if slots.is_empty()
        ));
    }

    #[test]
    fn callable_cut_is_semantic_and_ignores_private_abi_arity() {
        let public_param = product(nominal("api", "A"), nominal("api", "B"));
        let signature = Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
            "payload",
            public_param,
        )])]);
        let first = BoundaryCallablePlan::from_prepared_signature(
            &signature,
            &function(nominal("api", "C"), nominal("api", "R"), 1),
            &BTreeSet::new(),
        )
        .expect("first semantic callable cut");
        let second = BoundaryCallablePlan::from_prepared_signature(
            &signature,
            &function(nominal("api", "C"), nominal("api", "R"), 47),
            &BTreeSet::new(),
        )
        .expect("second semantic callable cut");

        assert_eq!(first, second);
        assert!(matches!(
            first.entry().head_stages[0],
            BoundaryCallableHeadStage::Value { slots } if slots.len() == 2
        ));
    }

    #[test]
    fn exact_signature_distinguishes_a_returned_function_from_a_second_value_group() {
        let one_group = Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
            "first",
            nominal("api", "A"),
        )])]);
        let one_group = BoundaryCallablePlan::from_prepared_signature(
            &one_group,
            &function(nominal("api", "B"), nominal("api", "R"), 1),
            &BTreeSet::new(),
        )
        .expect("one declaration-owned value group");
        let two_groups = Signature::from_groups(vec![
            SignatureGroup::Value(vec![value_param("first", nominal("api", "A"))]),
            SignatureGroup::Value(vec![value_param("second", nominal("api", "B"))]),
        ]);
        let two_groups = BoundaryCallablePlan::from_prepared_signature(
            &two_groups,
            &nominal("api", "R"),
            &BTreeSet::new(),
        )
        .expect("two declaration-owned value groups");

        assert_eq!(one_group.facade(), two_groups.facade());
        assert_eq!(
            one_group.head_stage_kinds(),
            &[BoundaryCallableHeadStageKind::Value]
        );
        assert_eq!(
            two_groups.head_stage_kinds(),
            &[
                BoundaryCallableHeadStageKind::Value,
                BoundaryCallableHeadStageKind::Value,
            ]
        );
        assert!(matches!(
            one_group.facade().use_at(one_group.entry().returned),
            FacadeUse::Function { .. }
        ));
        assert!(matches!(
            two_groups.facade().use_at(two_groups.entry().returned),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "R")
        ));
    }

    #[test]
    fn product_companion_preserves_exact_semantic_key_order() {
        let sum = FacadeShellId::new(
            FacadeKind::Sum,
            vec![
                SemanticKey::Qualified {
                    module_segments: vec!["api".to_owned(), "nested".to_owned()],
                    name: "Value".to_owned(),
                },
                SemanticKey::Positional { index: 1 },
            ],
        );
        assert_eq!(
            sum.product_companion(),
            FacadeShellId::new(FacadeKind::Product, sum.ordered_keys().to_vec())
        );
    }

    #[test]
    fn public_identity_codec_is_total_for_long_adversarial_keys() {
        let long = "long_segment_".repeat(4096);
        let identity = FacadeShellId::new(
            FacadeKind::Product,
            vec![
                SemanticKey::Bare {
                    name: "Product\0/_😀".to_owned(),
                },
                SemanticKey::Qualified {
                    module_segments: vec![
                        String::new(),
                        "a/b_c".to_owned(),
                        "λ".to_owned(),
                        long.clone(),
                    ],
                    name: "Sum".to_owned(),
                },
                SemanticKey::Positional { index: u64::MAX },
            ],
        );
        let encoded = identity.encode_public();
        assert!(encoded.len() > long.len());
        assert!(encoded.starts_with("KioFacade_V1_Product_K3_B"));
        assert!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        );
        assert_eq!(
            FacadeShellId::decode_public(&encoded).expect("codec round trip"),
            identity
        );
        let exact = identity.encode_exact_fallback();
        assert_eq!(
            FacadeShellId::decode_public(&exact).expect("exact fallback round trip"),
            identity
        );

        let product = FacadeShellId::new(
            FacadeKind::Product,
            vec![
                SemanticKey::Positional { index: 0 },
                SemanticKey::Positional { index: 1 },
            ],
        );
        assert_eq!(product.encode_public(), "Product");
        assert_eq!(
            FacadeShellId::decode_public("Product").expect("binary special"),
            product
        );

        let fixed = FacadeShellId::new(
            FacadeKind::Product,
            vec![SemanticKey::Bare {
                name: "A".to_owned(),
            }],
        );
        assert_eq!(fixed.encode_public(), "KioFacade_V1_Product_K1_B1_A");
        assert_eq!(
            FacadeShellId::decode_public("KioFacade_V1_Product_K1_B1_A")
                .expect("fixed readable codec vector"),
            fixed
        );
        assert_eq!(
            fixed.encode_exact_fallback(),
            "KioFacadeX_4b46530000000100000000000000000100000000000000000141"
        );
        assert_eq!(
            FacadeShellId::decode_public(
                "KioFacadeX_4b46530000000100000000000000000100000000000000000141"
            )
            .expect("fixed exact codec vector"),
            fixed
        );

        for malformed in [
            "",
            "KioFacade_V01_Product_K1_B1_A",
            "KioFacade_V2_Product_K1_B1_A",
            "KioFacade_V1_Product_K1_B2_A",
            "KioFacade_V1_Product_K1_B2__q",
            "KioFacadeX_0",
            "KioFacadeX_FF",
            "KioFacadeX_4b4653",
            "KioFacadeX_4b46530000000200000000000000000100000000000000000141",
            "KioFacadeX_4b465300000001000000000000000000ff",
            "Product_extra",
        ] {
            assert!(
                FacadeShellId::decode_public(malformed).is_err(),
                "{malformed:?} must be rejected"
            );
        }
    }

    fn shell_name(ty: &Type<Routed>, semantic: &BTreeSet<QualifiedTypeName>) -> String {
        let plan = BoundaryFacadePlan::from_prepared_scheme(prepared(ty, semantic));
        match plan.use_at(plan.root()) {
            FacadeUse::Product { shell, .. } | FacadeUse::Sum { shell, .. } => {
                shell.encode_public()
            }
            other => panic!("expected structural root, got {other:?}"),
        }
    }

    #[test]
    fn public_identity_is_independent_of_registration_order_and_unrelated_names() {
        let first = product(nominal("a", "Foo"), nominal("host", "Scalar"));
        let second = sum(nominal("b", "Bar"), nominal("host", "Scalar"));
        let own = BTreeSet::from([semantic_name("a", "Foo"), semantic_name("b", "Bar")]);
        let extended = BTreeSet::from([
            semantic_name("a", "Foo"),
            semantic_name("b", "Bar"),
            semantic_name("unrelated", "Noise"),
        ]);

        let forward = [("first", &first), ("second", &second)]
            .into_iter()
            .map(|(label, ty)| (label, shell_name(ty, &own)))
            .collect::<BTreeMap<_, _>>();
        let reverse = [("second", &second), ("first", &first)]
            .into_iter()
            .map(|(label, ty)| (label, shell_name(ty, &extended)))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(forward, reverse);
    }

    fn export_site(module: &str, name: &str) -> BoundaryFacadeSiteId {
        callable_site(
            module,
            BoundaryFacadeSiteOwner::ExportedFunction {
                name: name.to_owned(),
            },
        )
    }

    fn host_site(module: &str, name: &str) -> BoundaryFacadeSiteId {
        callable_site(
            module,
            BoundaryFacadeSiteOwner::HostFunction {
                name: name.to_owned(),
            },
        )
    }

    fn newtype_site(
        module: &str,
        newtype: &str,
        member: &str,
        role: NewtypeMemberRole,
    ) -> BoundaryFacadeSiteId {
        let owner = match role {
            NewtypeMemberRole::Constructor => BoundaryFacadeSiteOwner::NewtypeConstructor {
                newtype: newtype.to_owned(),
                member: member.to_owned(),
            },
            NewtypeMemberRole::Projector => BoundaryFacadeSiteOwner::NewtypeProjector {
                newtype: newtype.to_owned(),
                member: member.to_owned(),
            },
        };
        callable_site(module, owner)
    }

    fn callable_site(module: &str, owner: BoundaryFacadeSiteOwner) -> BoundaryFacadeSiteId {
        BoundaryFacadeSiteId::new(module.split('/').map(str::to_owned).collect(), owner)
            .expect("valid callable site")
    }

    fn exported_function(
        name: &str,
        signature: Signature<Routed>,
        ret: Type<Routed>,
    ) -> Item<Routed> {
        function_with_visibility(name, signature, ret, Visibility::Public)
    }

    fn function_with_visibility(
        name: &str,
        signature: Signature<Routed>,
        ret: Type<Routed>,
        vis: Visibility,
    ) -> Item<Routed> {
        Item::FnDef(FnDef {
            vis,
            purity: (),
            name: name.to_owned(),
            sig: signature,
            ret,
            ret_elided: (),
            body: Expr::Unit {
                occurrence: Default::default(),
                meta: meta(),
            },
            meta: meta(),
            doc: None,
        })
    }

    fn nullary_export(name: &str, ret: Type<Routed>) -> Item<Routed> {
        exported_function(name, Signature::from_groups(Vec::new()), ret)
    }

    fn host_function(name: &str, param: Type<Routed>, ret: Type<Routed>) -> Item<Routed> {
        Item::HostFn(HostFn {
            name: name.to_owned(),
            params: vec![HostFnParam::Value(HostFnValueParam {
                name: Some("value".to_owned()),
                ty: param,
                meta: meta(),
            })],
            param_groups: vec![crate::ast::SignatureGroupKind::Value { len: 1 }],
            ret,
            meta: meta(),
            doc: None,
        })
    }

    fn member(name: &str, vis: Visibility) -> TypeMember<Routed> {
        TypeMember {
            vis,
            name: name.to_owned(),
            span: span(),
            leading_trivia: (),
        }
    }

    fn public_newtype(name: &str, payload: Type<Routed>) -> Item<Routed> {
        newtype_with_visibility(
            name,
            payload,
            Visibility::Public,
            Visibility::Public,
            Visibility::Public,
        )
    }

    fn newtype_with_visibility(
        name: &str,
        payload: Type<Routed>,
        vis: Visibility,
        constructor_vis: Visibility,
        projector_vis: Visibility,
    ) -> Item<Routed> {
        Item::Newtype(Newtype {
            vis,
            rec_span: None,
            name: name.to_owned(),
            name_span: span(),
            type_params: Vec::new(),
            existential_params: Vec::new(),
            payload,
            constructor: member("wrap", constructor_vis),
            projector: member("unwrap", projector_vis),
            meta: meta(),
            editable_span: None,
            doc: None,
        })
    }

    fn public_alias(name: &str, type_params: Vec<TypeParam>, body: Type<Routed>) -> Item<Routed> {
        Item::TypeAlias(TypeAlias {
            vis: Visibility::Public,
            name: name.to_owned(),
            name_span: span(),
            type_params,
            body,
            meta: meta(),
            editable_span: None,
            doc: None,
        })
    }

    fn host_type(name: &str, type_params: Vec<TypeParam>, role: Option<Role>) -> Item<Routed> {
        Item::HostType(HostType {
            name: name.to_owned(),
            type_params,
            role: role.map(|role| RoleAnnotation { role, span: span() }),
            owned: false,
            meta: meta(),
            doc: None,
        })
    }

    fn roleless_host_type(name: &str) -> Item<Routed> {
        host_type(name, Vec::new(), None)
    }

    fn scoped_visibility(module_path: &str) -> Visibility {
        Visibility::PublicIn(ModulePath {
            segments: module_path
                .split('/')
                .map(|segment| PathSegment::synth(segment, span()))
                .collect(),
            span: span(),
        })
    }

    fn package_with_items(module_path: &str, items: Vec<Item<Routed>>) -> Package<Routed> {
        package_with_modules(vec![(module_path, items)])
    }

    fn package_with_modules(modules: Vec<(&str, Vec<Item<Routed>>)>) -> Package<Routed> {
        package_with_modules_and_bridge(modules, vec![vec![BridgeGlobSegment::DoubleStar]])
    }

    fn package_with_modules_and_bridge(
        modules: Vec<(&str, Vec<Item<Routed>>)>,
        bridge_globs: Vec<Vec<BridgeGlobSegment>>,
    ) -> Package<Routed> {
        let modules = modules
            .into_iter()
            .map(|(module_path, items)| {
                let path = ModulePath {
                    segments: module_path
                        .split('/')
                        .map(|segment| PathSegment::synth(segment, span()))
                        .collect(),
                    span: span(),
                };
                // `Package<Routed>` carries the declaration/import index from
                // its validated Prime predecessor. Keep this synthetic
                // package honest too: the index IDs must address the same
                // item positions as the Routed module below. The declaration
                // kind is immaterial to `TopLevelScope`; a host-type shell
                // preserves each exact name/span without inventing a second
                // Routed-phase scope builder for test fixtures.
                let scope_items = items
                    .iter()
                    .map(|item| {
                        if let Item::TypeRecGroup(group) = item {
                            return Item::TypeRecGroup(crate::ast::TypeRecGroup {
                                members: group
                                    .members
                                    .iter()
                                    .map(|member| {
                                        let (name, vis, span) = match member {
                                            crate::ast::TypeRecMember::TypeAlias(alias) => (
                                                alias.name.clone(),
                                                alias.vis.clone(),
                                                alias.meta.span,
                                            ),
                                            crate::ast::TypeRecMember::Newtype(newtype) => (
                                                newtype.name.clone(),
                                                newtype.vis.clone(),
                                                newtype.meta.span,
                                            ),
                                            crate::ast::TypeRecMember::Labels(_, ext) => {
                                                match *ext {}
                                            }
                                        };
                                        crate::ast::TypeRecMember::TypeAlias(TypeAlias {
                                            vis,
                                            name,
                                            name_span: span,
                                            type_params: Vec::new(),
                                            body: Type::Unit {
                                                meta: Meta::new(span),
                                            },
                                            meta: Meta::new(span),
                                            editable_span: None,
                                            doc: None,
                                        })
                                    })
                                    .collect(),
                                doc: group.doc.clone(),
                                source_layout: group.source_layout.clone(),
                                rec_span: group.rec_span,
                                open_brace_span: group.open_brace_span,
                                close_brace_span: group.close_brace_span,
                                deferred_rec_labels_diagnostic: group
                                    .deferred_rec_labels_diagnostic
                                    .clone(),
                                meta: Meta::new(group.meta.span),
                            });
                        }
                        let name = match item {
                            Item::FnDef(declaration) => declaration.name.clone(),
                            Item::TypeAlias(declaration) => declaration.name.clone(),
                            Item::Newtype(declaration) => declaration.name.clone(),
                            Item::HostType(declaration) => declaration.name.clone(),
                            Item::HostFn(declaration) => declaration.name.clone(),
                            Item::RecGroup(_, ext)
                            | Item::LiteralAlias(_, ext)
                            | Item::Labels(_, ext)
                            | Item::LabelForward(_, ext)
                            | Item::Equiv(_, ext)
                            | Item::Elaborator(_, ext)
                            | Item::Op(_, ext)
                            | Item::VariadicOperator(_, ext) => match *ext {},
                            Item::TypeRecGroup(_) => unreachable!("handled above"),
                        };
                        Item::HostType(HostType {
                            name,
                            type_params: Vec::new(),
                            role: None,
                            owned: false,
                            meta: Meta::new(item.span()),
                            doc: None,
                        })
                    })
                    .collect();
                let scope_module = Module::<Prime> {
                    path: path.clone(),
                    imports: Vec::new(),
                    items: scope_items,
                    meta: Meta::new(span()),
                    doc: None,
                };
                let scope = TopLevelScope::build(&scope_module).expect("matching scope module");
                let module = Module::<Routed> {
                    path,
                    imports: Vec::new(),
                    items,
                    meta: meta(),
                    doc: None,
                };
                (
                    module_path.to_owned(),
                    ModuleEntry {
                        file_path: PathBuf::from(format!("{module_path}.kio")),
                        module,
                        scope,
                    },
                )
            })
            .collect();
        let package_file = PackageFileEntry {
            file_path: PathBuf::from("fixture.pkg.kio"),
            package_name: "fixture".to_owned(),
            package_file: PackageFile {
                name: "fixture".to_owned(),
                build: None,
                bridge: Some(BridgeBlock {
                    globs: bridge_globs
                        .into_iter()
                        .map(|segments| BridgeGlob {
                            segments,
                            span: span(),
                            leading_trivia: Vec::new(),
                        })
                        .collect(),
                    span: span(),
                    leading_trivia: Vec::new(),
                    trailing_trivia: Vec::new(),
                }),
                meta: meta(),
            },
        };
        Package::from_parts(modules, Some(package_file))
    }

    fn replayed_interface(source: &str) -> ReplayedInterface {
        let file = crate::pass::parser::parse_signature_file(source, None)
            .unwrap_or_else(|error| panic!("failed to parse retained fixture: {error:?}"));
        crate::sig::replay(&file)
            .unwrap_or_else(|error| panic!("failed to replay retained fixture: {error:?}"))
    }

    /// Build a deliberately corrupt retained artifact after parsing so these
    /// tests can keep exercising boundary planning's independent fail-closed
    /// checks. Fresh signature input itself is validated by `sig::replay` and
    /// has separate causal coverage at that boundary.
    fn replayed_invalid_interface(source: &str) -> ReplayedInterface {
        let file = crate::pass::parser::parse_signature_file(source, None)
            .unwrap_or_else(|error| panic!("failed to parse invalid retained fixture: {error:?}"));
        crate::sig::replay_unvalidated_for_downstream_defense(&file).unwrap_or_else(|error| {
            panic!("failed to construct invalid retained fixture: {error:?}")
        })
    }

    fn removed_host_mut<'a>(
        replayed: &'a mut ReplayedInterface,
        module: &str,
        name: &str,
    ) -> &'a mut RemovedItem {
        replayed
            .removed
            .iter_mut()
            .find(|item| {
                item.entry.name == QualifiedName::new(module, name)
                    && item.entry.side == ContractSide::Env
                    && matches!(&item.entry.kind, ContractKind::Fn { .. })
            })
            .unwrap_or_else(|| panic!("missing removed host root {module}.{name}"))
    }

    fn removed_host<'a>(
        replayed: &'a ReplayedInterface,
        module: &str,
        name: &str,
    ) -> &'a RemovedItem {
        replayed
            .removed
            .iter()
            .find(|item| {
                item.entry.name == QualifiedName::new(module, name)
                    && item.entry.side == ContractSide::Env
                    && matches!(&item.entry.kind, ContractKind::Fn { .. })
            })
            .unwrap_or_else(|| panic!("missing removed host root {module}.{name}"))
    }

    fn refresh_removed_host_contract_signature(
        replayed: &mut ReplayedInterface,
        module: &str,
        name: &str,
    ) {
        let removed = removed_host_mut(replayed, module, name);
        let normalized = {
            let SigItem::HostFn(host) = &removed.frozen else {
                panic!("removed host root must freeze a host fn");
            };
            let closure = removed
                .frozen_type_closure
                .as_ref()
                .expect("removed host root has a frozen type closure");
            FrozenRootTypeResolver { closure }
                .canonical_host_signature(host, &removed.entry.name.module_path, &removed.imports)
                .expect("removed host root has consistent parameter groups")
        };
        let ContractKind::Fn { signature, .. } = &mut removed.entry.kind else {
            panic!("removed host root has a function contract");
        };
        *signature = normalized;
    }

    fn execution_value_stage<'a>(
        site: PreparedBoundaryCallableSite<'a>,
        index: usize,
    ) -> &'a CallableValueStageLayout {
        let Some(CallableExecutionStage::Value(layout)) = site
            .execution()
            .expect("live site")
            .head_stages()
            .get(index)
        else {
            panic!("expected execution value stage at index {index}");
        };
        layout
    }

    fn nominal_declaration<'a>(
        site: PreparedBoundaryCallableSite<'a>,
        module: &str,
        name: &str,
    ) -> &'a BoundaryNominalDeclaration {
        site.nominals()
            .declaration(&semantic_name(module, name))
            .unwrap_or_else(|| panic!("missing projected nominal {module}.{name}"))
    }

    fn newtype_payload<'a>(
        site: PreparedBoundaryCallableSite<'a>,
        module: &str,
        name: &str,
    ) -> &'a BoundaryNewtypePayloadPlan {
        let BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        } = nominal_declaration(site, module, name)
        else {
            panic!("projected nominal {module}.{name} has no transparent payload")
        };
        payload
    }

    fn assert_alias_substitution_payload_is_capture_free(payload: &BoundaryNewtypePayloadPlan) {
        assert_eq!(payload.declaration_binders().len(), 1);
        let declaration = payload.declaration_binders()[0];
        let declaration_binder = payload.facade().binder(declaration);
        assert_eq!(declaration_binder.name, "A");
        assert_eq!(declaration_binder.kind, Kind::Star);

        let (outer, after_outer) = expect_forall(payload.facade(), payload.payload_root());
        let (inner, body) = expect_forall(payload.facade(), after_outer);
        assert_eq!(payload.facade().binder(outer).name, "A_n2");
        assert_eq!(payload.facade().binder(outer).kind, Kind::Star);
        assert_eq!(payload.facade().binder(inner).name, "A_n3");
        assert_eq!(payload.facade().binder(inner).kind, Kind::Star);
        assert_ne!(declaration, outer);
        assert_ne!(declaration, inner);
        assert_ne!(outer, inner);

        let (_, slots) = expect_product(payload.facade(), body);
        assert_eq!(slots.len(), 3);
        for (slot, expected) in slots.iter().zip([declaration, outer, inner]) {
            assert!(matches!(
                payload.facade().use_at(*slot),
                FacadeUse::Bound { binder, .. } if binder == &expected
            ));
        }
    }

    fn assert_plan_nominals_are_site_local(site: PreparedBoundaryCallableSite<'_>) {
        for use_ in site.plan().facade().uses() {
            if let FacadeUse::Nominal { name, .. } = use_ {
                assert!(
                    site.nominals().declaration(name).is_some(),
                    "plan nominal {name:?} is absent from its site's projection"
                );
            }
        }
        for (_, declaration) in site.nominals().declarations() {
            let BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            } = declaration
            else {
                continue;
            };
            for name in facade_nominal_names(payload.facade()) {
                assert!(
                    site.nominals().declaration(&name).is_some(),
                    "payload nominal {name:?} is absent from its site's projection"
                );
            }
        }
    }

    #[test]
    fn live_collection_is_order_independent_and_shares_only_shell_identity() {
        let first = || {
            nullary_export(
                "first",
                product(nominal("payload", "Alpha"), nominal("payload", "Beta")),
            )
        };
        let second = || {
            nullary_export(
                "second",
                product(nominal("payload", "Gamma"), nominal("payload", "Delta")),
            )
        };
        let bridge = || vec![vec![BridgeGlobSegment::Literal("api".to_owned())]];
        let forward_package = package_with_modules_and_bridge(
            vec![
                ("api", vec![first(), second()]),
                (
                    "payload",
                    vec![
                        roleless_host_type("Alpha"),
                        roleless_host_type("Beta"),
                        roleless_host_type("Gamma"),
                        roleless_host_type("Delta"),
                    ],
                ),
                (
                    "other",
                    vec![nullary_export("third", nominal("host", "Value"))],
                ),
            ],
            bridge(),
        );
        let reverse_package = package_with_modules_and_bridge(
            vec![
                (
                    "other",
                    vec![nullary_export("third", nominal("host", "Value"))],
                ),
                (
                    "payload",
                    vec![
                        roleless_host_type("Delta"),
                        roleless_host_type("Gamma"),
                        roleless_host_type("Beta"),
                        roleless_host_type("Alpha"),
                    ],
                ),
                ("api", vec![second(), first()]),
            ],
            bridge(),
        );
        let forward = PreparedBoundaryCallableSites::collect_live(&forward_package)
            .expect("forward live sites");
        let reverse = PreparedBoundaryCallableSites::collect_live(&reverse_package)
            .expect("reverse live sites");

        assert_eq!(forward, reverse);
        assert!(forward.site(&export_site("api", "first")).is_some());
        assert!(forward.site(&export_site("api", "second")).is_some());
        assert_eq!(forward.sites().len(), 2);
        assert_eq!(
            forward.shells().cloned().collect::<Vec<_>>(),
            vec![FacadeShellId::new(
                FacadeKind::Product,
                vec![
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ],
            )]
        );
    }

    fn wide_export_collection_work(width: usize) -> (LiveBoundaryCollectionWork, usize, usize) {
        let mut items = (0..width)
            .map(|index| {
                nullary_export(
                    &format!("export{index:03}"),
                    nominal("api", &format!("Host{index:03}")),
                )
            })
            .collect::<Vec<_>>();
        items.extend((0..width).map(|index| roleless_host_type(&format!("Host{index:03}"))));
        let package = package_with_items("api", items);
        let (prepared, work) = PreparedBoundaryCallableSites::collect_live_with_work(&package);
        let prepared = prepared.expect("wide live boundary collection");
        assert_eq!(prepared.sites().len(), width);

        let item_count = width * 2;
        let initial_surface_build_item_scans = item_count;
        let callable_inventory_item_scans = item_count;
        let legacy_callable_item_scans = (1..=width).sum::<usize>();
        let legacy_per_site_surface_rebuild_item_scans = width * item_count;
        let legacy_nominal_item_scans = width * item_count;
        let legacy_item_scans = initial_surface_build_item_scans
            + callable_inventory_item_scans
            + legacy_callable_item_scans
            + legacy_per_site_surface_rebuild_item_scans
            + legacy_nominal_item_scans;
        let indexed_item_scans = work.nominal_declaration_index_item_visits
            + work.callable_inventory_item_visits
            + item_count * work.public_newtype_surface_builds;
        (work, legacy_item_scans, indexed_item_scans)
    }

    fn wide_newtype_collection_work(width: usize) -> (LiveBoundaryCollectionWork, usize, usize) {
        let package = package_with_items(
            "api",
            (0..width)
                .map(|index| public_newtype(&format!("Wrapper{index:03}"), unit()))
                .collect(),
        );
        let (prepared, work) = PreparedBoundaryCallableSites::collect_live_with_work(&package);
        let prepared = prepared.expect("wide newtype boundary collection");
        assert_eq!(prepared.sites().len(), width * 2);

        let item_count = width;
        let site_count = width * 2;
        let initial_surface_build_item_scans = item_count;
        let callable_inventory_item_scans = item_count;
        let legacy_member_lookup_item_scans = site_count * (width + 1) / 2;
        let legacy_per_site_surface_rebuild_item_scans = site_count * item_count;
        let legacy_nominal_item_scans = site_count * item_count;
        let legacy_item_scans = initial_surface_build_item_scans
            + callable_inventory_item_scans
            + legacy_member_lookup_item_scans
            + legacy_per_site_surface_rebuild_item_scans
            + legacy_nominal_item_scans;
        let indexed_item_scans = work.nominal_declaration_index_item_visits
            + work.callable_inventory_item_visits
            + item_count * work.public_newtype_surface_builds;
        (work, legacy_item_scans, indexed_item_scans)
    }

    #[test]
    fn live_collection_indexes_declarations_once_instead_of_rescanning_per_site() {
        let (small, legacy_small, indexed_small) = wide_export_collection_work(8);
        let (large, legacy_large, indexed_large) = wide_export_collection_work(32);

        for (width, work) in [(8, small), (32, large)] {
            assert_eq!(work.public_newtype_surface_builds, 1);
            assert_eq!(work.nominal_declaration_index_builds, 1);
            assert_eq!(work.nominal_declaration_index_item_visits, width * 2);
            assert_eq!(work.callable_inventory_item_visits, width * 2);
            assert_eq!(work.direct_callable_preparations, width);
            assert_eq!(work.by_name_callable_resolutions, 0);
            assert_eq!(work.nominal_declaration_lookups, width);
        }
        assert_eq!((legacy_small, legacy_large), (324, 4_752));
        assert_eq!((indexed_small, indexed_large), (48, 192));
        assert!(legacy_large > legacy_small * 14);
        assert_eq!(indexed_large, indexed_small * 4);
        assert_eq!(
            large.nominal_declaration_lookups,
            small.nominal_declaration_lookups * 4
        );
        assert!(legacy_large > indexed_large * 20);

        let (newtypes_small, newtypes_legacy_small, newtypes_indexed_small) =
            wide_newtype_collection_work(8);
        let (newtypes_large, newtypes_legacy_large, newtypes_indexed_large) =
            wide_newtype_collection_work(32);
        for (width, work) in [(8, newtypes_small), (32, newtypes_large)] {
            assert_eq!(work.public_newtype_surface_builds, 1);
            assert_eq!(work.nominal_declaration_index_builds, 1);
            assert_eq!(work.nominal_declaration_index_item_visits, width);
            assert_eq!(work.callable_inventory_item_visits, width);
            assert_eq!(work.direct_callable_preparations, width * 2);
            assert_eq!(work.by_name_callable_resolutions, 0);
            assert_eq!(work.nominal_declaration_lookups, width * 2);
        }
        assert_eq!((newtypes_legacy_small, newtypes_legacy_large), (344, 5_216));
        assert_eq!((newtypes_indexed_small, newtypes_indexed_large), (24, 96));
        assert!(newtypes_legacy_large > newtypes_legacy_small * 15);
        assert_eq!(newtypes_indexed_large, newtypes_indexed_small * 4);
        assert_eq!(
            newtypes_large.nominal_declaration_lookups,
            newtypes_small.nominal_declaration_lookups * 4
        );
        assert!(newtypes_legacy_large > newtypes_indexed_large * 20);
    }

    #[test]
    fn duplicate_site_always_fails_closed() {
        let package = package_with_modules(vec![
            (
                "api",
                vec![nullary_export(
                    "entry",
                    product(nominal("payload", "Alpha"), nominal("payload", "Beta")),
                )],
            ),
            (
                "payload",
                vec![roleless_host_type("Alpha"), roleless_host_type("Beta")],
            ),
        ]);
        let semantic_nominals = live_semantic_nominals(&package);
        let nominal_declarations = live_nominal_declaration_index(&package);
        let mut collector = PreparedBoundaryCallableSitesCollector::empty();
        collector
            .insert_live(
                prepare_exported_boundary_callable(&package, "api", "entry")
                    .expect("owner-coupled export"),
                &semantic_nominals,
                &package,
                &nominal_declarations,
            )
            .expect("first collection");
        let error = collector
            .insert_live(
                prepare_exported_boundary_callable(&package, "api", "entry")
                    .expect("same owner-coupled export"),
                &semantic_nominals,
                &package,
                &nominal_declarations,
            )
            .expect_err("any duplicate declaration identity must fail");
        let BoundaryFacadeCollectionError::Duplicate(error) = error else {
            panic!("expected duplicate-site error");
        };
        assert_eq!(error.site(), &export_site("api", "entry"));

        let prepared = collector.finish().expect("valid empty inventory");
        assert_eq!(prepared.sites().len(), 1);
        assert_eq!(prepared.shells().len(), 1);
    }

    #[test]
    fn standalone_opaque_newtype_is_inventoried_without_a_callable_site() {
        let mut token = newtype_with_visibility(
            "Token",
            unit(),
            Visibility::Public,
            Visibility::Private,
            Visibility::Private,
        );
        let Item::Newtype(token_declaration) = &mut token else {
            unreachable!("the fixture helper constructs a newtype")
        };
        token_declaration.type_params.push(TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        });
        token_declaration.existential_params.push(TypeParam {
            name: "F".to_owned(),
            span: span(),
            kind: Some(Kind::Arrow(Box::new(Kind::Star), Box::new(Kind::Star))),
        });
        let package = package_with_items("api", vec![token]);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("standalone opaque inventory");
        let entries = prepared.public_newtypes().collect::<Vec<_>>();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), &semantic_name("api", "Token"));
        assert!(matches!(
            entries[0].surface(),
            BoundaryNewtypeSurface::Opaque
        ));
        assert_eq!(entries[0].type_params().len(), 1);
        assert_eq!(entries[0].type_params()[0].name(), "A");
        assert_eq!(entries[0].type_params()[0].kind(), &Kind::Star);
        assert_eq!(entries[0].existential_params().len(), 1);
        assert_eq!(entries[0].existential_params()[0].name(), "F");
        assert_eq!(
            entries[0].existential_params()[0].kind(),
            &Kind::Arrow(Box::new(Kind::Star), Box::new(Kind::Star))
        );
        assert_eq!(prepared.sites().len(), 0);
    }

    #[test]
    fn public_newtype_inventory_and_member_sites_fail_closed_when_separated() {
        let package = package_with_items(
            "api",
            vec![newtype_with_visibility(
                "Token",
                unit(),
                Visibility::Public,
                Visibility::Public,
                Visibility::Private,
            )],
        );
        let name = semantic_name("api", "Token");
        let inventory = BTreeMap::from([(
            name.clone(),
            BoundaryPublicNewtypeInventoryEntry {
                name: name.clone(),
                type_params: Vec::new(),
                existential_params: Vec::new(),
                surface: BoundaryNewtypeSurface::Constructor {
                    member: "wrap".to_owned(),
                },
            },
        )]);
        assert!(matches!(
            PreparedBoundaryCallableSitesCollector::with_public_newtypes(inventory).finish(),
            Err(BoundaryFacadeCollectionError::InvalidPublicNewtypeInventory {
                name: actual,
                ..
            }) if actual == name
        ));

        let semantic_nominals = live_semantic_nominals(&package);
        let nominal_declarations = live_nominal_declaration_index(&package);
        let mut unlisted = PreparedBoundaryCallableSitesCollector::empty();
        unlisted
            .insert_live(
                prepare_newtype_constructor_boundary_callable(&package, "api", "Token")
                    .expect("authoritative constructor"),
                &semantic_nominals,
                &package,
                &nominal_declarations,
            )
            .expect("member site preparation");
        assert!(matches!(
            unlisted.finish(),
            Err(BoundaryFacadeCollectionError::InvalidPublicNewtypeInventory {
                name: actual,
                ..
            }) if actual == name
        ));
    }

    #[test]
    fn live_nominal_projection_preserves_roles_kinds_and_transitive_newtypes() {
        let type_param = TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        };
        let Item::Newtype(mut wrapper) = public_newtype(
            "Wrapper",
            product(
                bare("A"),
                product(nominal("nested", "Inner"), bare("Hidden")),
            ),
        ) else {
            unreachable!("the helper returns a newtype")
        };
        wrapper.type_params.push(type_param.clone());
        wrapper.existential_params.push(TypeParam {
            name: "Hidden".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        });
        let boundary = product(
            nominal("types", "Count"),
            product(
                nominal("types", "Token"),
                product(
                    nominal_args("types", "Box", vec![unit()]),
                    nominal_args("api", "Wrapper", vec![unit()]),
                ),
            ),
        );
        let package = package_with_modules(vec![
            (
                "api",
                vec![
                    Item::Newtype(wrapper),
                    exported_function(
                        "inspect",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                            "value", boundary,
                        )])]),
                        unit(),
                    ),
                ],
            ),
            (
                "nested",
                vec![public_newtype("Inner", nominal("types", "Token"))],
            ),
            (
                "types",
                vec![
                    host_type("Count", Vec::new(), Some(Role::I32)),
                    roleless_host_type("Token"),
                    host_type("Box", vec![type_param], None),
                ],
            ),
        ]);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("complete live nominal projections");
        let inspect = prepared
            .site(&export_site("api", "inspect"))
            .expect("inspect site");
        assert_plan_nominals_are_site_local(inspect);

        assert!(matches!(
            nominal_declaration(inspect, "types", "Count"),
            BoundaryNominalDeclaration::HostType {
                type_params,
                binding: BoundaryHostTypeBinding::Role(Role::I32),
            } if type_params.is_empty()
        ));
        assert!(matches!(
            nominal_declaration(inspect, "types", "Token"),
            BoundaryNominalDeclaration::HostType {
                type_params,
                binding: BoundaryHostTypeBinding::Roleless,
            } if type_params.is_empty()
        ));
        assert!(matches!(
            nominal_declaration(inspect, "types", "Box"),
            BoundaryNominalDeclaration::HostType {
                type_params,
                binding: BoundaryHostTypeBinding::Roleless,
            } if type_params.len() == 1
                && type_params[0].name() == "A"
                && type_params[0].kind() == &Kind::Star
        ));
        assert!(matches!(
            nominal_declaration(inspect, "api", "Wrapper"),
            BoundaryNominalDeclaration::Newtype {
                type_params,
                existential_params,
                transparent_payload: Some(_),
                surface: BoundaryNewtypeSurface::Both { .. },
                ..
            } if type_params.len() == 1
                && type_params[0].kind() == &Kind::Star
                && existential_params.len() == 1
                && existential_params[0].name() == "Hidden"
        ));
        assert!(matches!(
            nominal_declaration(inspect, "nested", "Inner"),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(_),
                surface: BoundaryNewtypeSurface::Both { .. },
                ..
            }
        ));
        assert_eq!(inspect.nominals().declarations().len(), 5);
    }

    #[test]
    fn live_and_retained_payload_alias_substitution_reserves_body_free_names() {
        let alias = public_alias(
            "Capture",
            vec![TypeParam {
                name: "X".to_owned(),
                span: span(),
                kind: Some(Kind::Star),
            }],
            forall(
                "A_n2",
                forall("A", product(bare("X"), product(bare("A_n2"), bare("A")))),
            ),
        );
        let Item::Newtype(mut payload) =
            public_newtype("Payload", nominal_args("api", "Capture", vec![bare("A")]))
        else {
            unreachable!("the helper constructs a newtype")
        };
        payload.type_params.push(TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        });
        let live_package = package_with_items("api", vec![alias, Item::Newtype(payload)]);
        let live = PreparedBoundaryCallableSites::collect_live(&live_package)
            .expect("capture-free live alias expansion");
        let live_site = live
            .site(&newtype_site(
                "api",
                "Payload",
                "wrap",
                NewtypeMemberRole::Constructor,
            ))
            .expect("live payload constructor");
        assert_alias_substitution_payload_is_capture_free(newtype_payload(
            live_site, "api", "Payload",
        ));

        let retained = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        type Capture[X] = [A_n2][A] (X & A_n2 & A);
        newtype Payload[A] : Capture(A) {
          pub constructor wrap;
          pub projector unwrap;
        };
        host fn consume[A](value: Payload(A)) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        consume;
      }
    }
  }
}
"#,
        );
        let empty = package_with_items("api", Vec::new());
        let retained = PreparedBoundaryCallableSites::collect(&empty, Some(&retained))
            .expect("capture-free retained alias expansion");
        let retained_site = retained
            .site(&host_site("api", "consume"))
            .expect("retained payload root");
        assert!(retained_site.execution().is_none());
        assert_alias_substitution_payload_is_capture_free(newtype_payload(
            retained_site,
            "api",
            "Payload",
        ));
    }

    #[test]
    fn live_boundary_snapshot_indexes_recursive_group_aliases_and_newtypes() {
        let Item::TypeAlias(value) = public_alias("Value", Vec::new(), nominal("api", "Node"))
        else {
            unreachable!("the helper constructs a type alias")
        };
        let Item::Newtype(node) = public_newtype("Node", nominal("api", "Value")) else {
            unreachable!("the helper constructs a newtype")
        };
        let group = Item::TypeRecGroup(crate::ast::TypeRecGroup {
            members: vec![
                crate::ast::TypeRecMember::TypeAlias(value),
                crate::ast::TypeRecMember::Newtype(node),
            ],
            doc: None,
            source_layout: None,
            rec_span: Some(span()),
            open_brace_span: Some(span()),
            close_brace_span: Some(span()),
            deferred_rec_labels_diagnostic: None,
            meta: meta(),
        });
        let package = package_with_items(
            "api",
            vec![
                group,
                nullary_export("apply_default", nominal("api", "Value")),
            ],
        );

        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("recursive-group declarations belong to the live nominal snapshot");
        let apply_default = prepared
            .site(&export_site("api", "apply_default"))
            .expect("exported function site");
        assert!(matches!(
            nominal_declaration(apply_default, "api", "Node"),
            BoundaryNominalDeclaration::Newtype { .. }
        ));
        assert_eq!(
            apply_default.nominals().declarations().len(),
            1,
            "the transparent grouped alias must expand to its grouped nominal anchor"
        );
    }

    #[test]
    fn payload_declaration_binders_keep_universals_before_existentials() {
        let Item::Newtype(mut poly) = public_newtype(
            "Poly",
            product(bare_args("F", vec![bare("Hidden")]), bare("Hidden")),
        ) else {
            unreachable!("the helper constructs a newtype")
        };
        poly.type_params.push(TypeParam {
            name: "F".to_owned(),
            span: span(),
            kind: Some(Kind::arrow_chain(1)),
        });
        poly.existential_params.push(TypeParam {
            name: "Hidden".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        });
        let package = package_with_items("api", vec![Item::Newtype(poly)]);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("payload declaration binder order");
        let site = prepared
            .site(&newtype_site(
                "api",
                "Poly",
                "wrap",
                NewtypeMemberRole::Constructor,
            ))
            .expect("polymorphic constructor site");
        let payload = newtype_payload(site, "api", "Poly");
        let [universal, existential] = payload.declaration_binders() else {
            panic!("one universal and one existential binder")
        };
        assert_ne!(universal, existential);
        assert_eq!(payload.facade().binder(*universal).name, "F");
        assert_eq!(
            payload.facade().binder(*universal).kind,
            Kind::arrow_chain(1)
        );
        assert_eq!(payload.facade().binder(*existential).name, "Hidden");
        assert_eq!(payload.facade().binder(*existential).kind, Kind::Star);

        let (_, slots) = expect_product(payload.facade(), payload.payload_root());
        assert_eq!(slots.len(), 2);
        let FacadeUse::Apply {
            constructor, args, ..
        } = payload.facade().use_at(slots[0])
        else {
            panic!("higher-kinded universal remains an application")
        };
        assert!(matches!(
            payload.facade().use_at(*constructor),
            FacadeUse::Bound { binder, .. } if binder == universal
        ));
        assert!(matches!(
            payload.facade().use_at(args[0]),
            FacadeUse::Bound { binder, .. } if binder == existential
        ));
        assert!(matches!(
            payload.facade().use_at(slots[1]),
            FacadeUse::Bound { binder, .. } if binder == existential
        ));
    }

    #[test]
    fn live_payload_builtin_spellings_erase_unless_semantically_shadowed() {
        let Item::Newtype(mut binder_shadow) =
            public_newtype("BinderShadow", bare("Comptime_bool"))
        else {
            unreachable!("the helper constructs a newtype")
        };
        binder_shadow.type_params.push(TypeParam {
            name: "Comptime_bool".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        });
        let package = package_with_modules(vec![
            (
                "erase",
                vec![
                    public_newtype("Erased", product(bare("__Type__"), bare("Comptime_bool"))),
                    exported_function(
                        "erase_fn",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                            "value",
                            bare("__Type__"),
                        )])]),
                        bare("Comptime_bool"),
                    ),
                ],
            ),
            ("binder", vec![Item::Newtype(binder_shadow)]),
            (
                "shadow",
                vec![
                    roleless_host_type("Comptime_bool"),
                    public_newtype("NominalShadow", bare("Comptime_bool")),
                ],
            ),
        ]);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("scope-aware live payload builtin erasure");

        let erased_site = prepared
            .site(&newtype_site(
                "erase",
                "Erased",
                "wrap",
                NewtypeMemberRole::Constructor,
            ))
            .expect("erased payload site");
        let erased = newtype_payload(erased_site, "erase", "Erased");
        let (_, erased_slots) = expect_product(erased.facade(), erased.payload_root());
        assert!(matches!(
            erased.facade().use_at(erased_slots[0]),
            FacadeUse::Bottom { .. }
        ));
        assert!(matches!(
            erased.facade().use_at(erased_slots[1]),
            FacadeUse::Unit { .. }
        ));

        let erased_fn = prepared
            .site(&export_site("erase", "erase_fn"))
            .expect("erased callable site");
        let BoundaryCallableHeadStage::Value { slots } = erased_fn.plan().entry().head_stages[0]
        else {
            panic!("the exported function has one value head")
        };
        let [erased_param] = slots else {
            panic!("the reflected type input remains one semantic slot")
        };
        assert!(matches!(
            erased_fn.plan().facade().use_at(*erased_param),
            FacadeUse::Bottom { .. }
        ));
        assert!(matches!(
            erased_fn
                .plan()
                .facade()
                .use_at(erased_fn.plan().entry().returned),
            FacadeUse::Unit { .. }
        ));

        let binder_site = prepared
            .site(&newtype_site(
                "binder",
                "BinderShadow",
                "wrap",
                NewtypeMemberRole::Constructor,
            ))
            .expect("binder-shadow payload site");
        let binder_payload = newtype_payload(binder_site, "binder", "BinderShadow");
        let binder = binder_payload.declaration_binders()[0];
        assert!(matches!(
            binder_payload.facade().use_at(binder_payload.payload_root()),
            FacadeUse::Bound { binder: actual, .. } if actual == &binder
        ));

        let nominal_site = prepared
            .site(&newtype_site(
                "shadow",
                "NominalShadow",
                "wrap",
                NewtypeMemberRole::Constructor,
            ))
            .expect("nominal-shadow payload site");
        let nominal_payload = newtype_payload(nominal_site, "shadow", "NominalShadow");
        assert!(matches!(
            nominal_payload
                .facade()
                .use_at(nominal_payload.payload_root()),
            FacadeUse::Nominal { name, .. }
                if name == &semantic_name("shadow", "Comptime_bool")
        ));
    }

    #[test]
    fn transparent_payload_execution_marks_synthetic_binders_without_invoking_them() {
        let carrier = newtype_with_visibility(
            "Carrier",
            unit(),
            Visibility::Public,
            Visibility::Public,
            Visibility::Private,
        );
        let Item::Newtype(mut payload) = public_newtype(
            "Payload",
            forall(
                "Runtime",
                function(
                    bare("Runtime"),
                    function(
                        bare("Hidden"),
                        product(bare("A"), nominal("api", "Carrier")),
                        1,
                    ),
                    1,
                ),
            ),
        ) else {
            unreachable!("the helper constructs a newtype")
        };
        payload.type_params.push(TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        });
        payload.existential_params.push(TypeParam {
            name: "Hidden".to_owned(),
            span: span(),
            kind: Some(Kind::Star),
        });
        let package = package_with_items(
            "api",
            vec![
                carrier,
                Item::Newtype(payload),
                nullary_export("witness", nominal_args("api", "Payload", vec![unit()])),
            ],
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("live transparent execution transaction");
        let site = prepared
            .site(&export_site("api", "witness"))
            .expect("transparent payload witness");
        let payload = newtype_payload(site, "api", "Payload");
        let execution = site.execution().expect("live witness execution");
        assert_eq!(
            execution
                .transparent_payloads()
                .map(|(name, _)| name.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([semantic_name("api", "Payload")])
        );
        assert!(
            execution
                .transparent_payload(&semantic_name("api", "Carrier"))
                .is_none()
        );
        let payload_execution = execution
            .transparent_payload(&semantic_name("api", "Payload"))
            .expect("Both payload execution");
        payload_execution
            .validate_alignment(payload.facade())
            .expect("payload execution alignment");

        let mut current = payload.facade().root();
        for expected_binder in payload.declaration_binders() {
            let FacadeUse::Forall { binder, result, .. } = payload.facade().use_at(current) else {
                panic!("synthetic declaration binder remains a leading forall")
            };
            assert_eq!(binder, expected_binder);
            assert!(matches!(
                payload_execution.use_at(current),
                BoundaryFacadeExecutionUse::DeclarationBinder
            ));
            current = *result;
        }
        assert_eq!(current, payload.payload_root());
        let (_, outer_function) = expect_forall(payload.facade(), current);
        assert!(matches!(
            payload_execution.use_at(current),
            BoundaryFacadeExecutionUse::InvokeForall
        ));
        let (_, inner_function) = expect_function(payload.facade(), outer_function);
        assert!(matches!(
            payload_execution.use_at(outer_function),
            BoundaryFacadeExecutionUse::Function(_)
        ));
        assert!(matches!(
            payload.facade().use_at(inner_function),
            FacadeUse::Function { .. }
        ));
        assert!(matches!(
            payload_execution.use_at(inner_function),
            BoundaryFacadeExecutionUse::Function(_)
        ));
    }

    #[test]
    fn payload_shell_dependencies_stay_site_owned_until_a_backend_selects_them() {
        let package = package_with_modules_and_bridge(
            vec![
                (
                    "api",
                    vec![exported_function(
                        "round",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                            "value",
                            nominal("types", "Outer"),
                        )])]),
                        nominal("types", "Hidden"),
                    )],
                ),
                (
                    "types",
                    vec![
                        roleless_host_type("Token"),
                        public_newtype("Inner", sum(nominal("types", "Token"), unit())),
                        public_newtype("Outer", nominal("types", "Inner")),
                        newtype_with_visibility(
                            "Hidden",
                            product(nominal("types", "Token"), unit()),
                            Visibility::Public,
                            Visibility::Private,
                            Visibility::Private,
                        ),
                    ],
                ),
            ],
            vec![
                vec![BridgeGlobSegment::Literal("api".to_owned())],
                vec![BridgeGlobSegment::Literal("types".to_owned())],
            ],
        );
        let semantic_nominals = live_semantic_nominals(&package);
        let nominal_declarations = live_nominal_declaration_index(&package);
        let mut collector = PreparedBoundaryCallableSitesCollector::empty();
        collector
            .insert_live(
                prepare_exported_boundary_callable(&package, "api", "round")
                    .expect("owner-coupled export"),
                &semantic_nominals,
                &package,
                &nominal_declarations,
            )
            .expect("site-owned payload dependencies");
        let prepared = collector.finish().expect("valid empty inventory");
        let round = prepared
            .site(&export_site("api", "round"))
            .expect("round site");
        assert_plan_nominals_are_site_local(round);

        assert!(matches!(
            nominal_declaration(round, "types", "Outer"),
            BoundaryNominalDeclaration::Newtype {
                surface: BoundaryNewtypeSurface::Both { .. },
                ..
            }
        ));
        assert!(matches!(
            nominal_declaration(round, "types", "Hidden"),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: None,
                surface: BoundaryNewtypeSurface::Opaque,
                ..
            }
        ));
        assert_eq!(
            newtype_payload(round, "types", "Inner")
                .shell_dependencies()
                .map(FacadeShellId::kind)
                .collect::<Vec<_>>(),
            vec![FacadeKind::Sum]
        );
        assert!(prepared.shells().next().is_none());
    }

    #[test]
    fn transparent_payload_cycles_close_per_site_and_stop_at_carriers() {
        let carrier = newtype_with_visibility(
            "Carrier",
            nominal("types", "Cut"),
            Visibility::Public,
            Visibility::Public,
            Visibility::Private,
        );
        let package = package_with_modules(vec![
            (
                "api",
                vec![
                    nullary_export("self_cycle", nominal("types", "SelfCycle")),
                    nullary_export("mutual_cycle", nominal("types", "Left")),
                    nullary_export("carrier_cycle", nominal("types", "Cut")),
                ],
            ),
            (
                "types",
                vec![
                    public_newtype("SelfCycle", nominal("types", "SelfCycle")),
                    public_newtype("Left", nominal("types", "Right")),
                    public_newtype("Right", nominal("types", "Left")),
                    public_newtype("Cut", nominal("types", "Carrier")),
                    carrier,
                ],
            ),
        ]);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("recursive public payloads are finite projections");

        let self_cycle = prepared
            .site(&export_site("api", "self_cycle"))
            .expect("self-recursive witness");
        assert_eq!(
            self_cycle
                .nominals()
                .declarations()
                .map(|(name, _)| name.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([semantic_name("types", "SelfCycle")])
        );
        assert_eq!(
            facade_nominal_names(newtype_payload(self_cycle, "types", "SelfCycle").facade()),
            BTreeSet::from([semantic_name("types", "SelfCycle")])
        );

        let mutual = prepared
            .site(&export_site("api", "mutual_cycle"))
            .expect("mutually recursive witness");
        assert_eq!(
            mutual
                .nominals()
                .declarations()
                .map(|(name, _)| name.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                semantic_name("types", "Left"),
                semantic_name("types", "Right"),
            ])
        );
        assert_eq!(
            facade_nominal_names(newtype_payload(mutual, "types", "Left").facade()),
            BTreeSet::from([semantic_name("types", "Right")])
        );
        assert_eq!(
            facade_nominal_names(newtype_payload(mutual, "types", "Right").facade()),
            BTreeSet::from([semantic_name("types", "Left")])
        );

        let cut = prepared
            .site(&export_site("api", "carrier_cycle"))
            .expect("carrier-cut witness");
        assert_eq!(
            cut.nominals()
                .declarations()
                .map(|(name, _)| name.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                semantic_name("types", "Carrier"),
                semantic_name("types", "Cut"),
            ])
        );
        assert_eq!(
            facade_nominal_names(newtype_payload(cut, "types", "Cut").facade()),
            BTreeSet::from([semantic_name("types", "Carrier")])
        );
        assert!(matches!(
            nominal_declaration(cut, "types", "Carrier"),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: None,
                surface: BoundaryNewtypeSurface::Constructor { .. },
                ..
            }
        ));
    }

    #[test]
    fn live_nominal_projection_preserves_all_newtype_surface_states() {
        let hidden = public_newtype("Unexposed", unit());
        let opaque = newtype_with_visibility(
            "Opaque",
            unit(),
            Visibility::Public,
            Visibility::Private,
            Visibility::Private,
        );
        let constructor = newtype_with_visibility(
            "Constructor",
            unit(),
            Visibility::Public,
            Visibility::Public,
            Visibility::Private,
        );
        let projector = newtype_with_visibility(
            "Projector",
            unit(),
            Visibility::Public,
            Visibility::Private,
            Visibility::Public,
        );
        let both = public_newtype("Both", unit());
        let boundary = product(
            nominal("hidden", "Unexposed"),
            product(
                nominal("api", "Opaque"),
                product(
                    nominal("api", "Constructor"),
                    product(nominal("api", "Projector"), nominal("api", "Both")),
                ),
            ),
        );
        let package = package_with_modules_and_bridge(
            vec![
                (
                    "api",
                    vec![
                        opaque,
                        constructor,
                        projector,
                        both,
                        nullary_export("surfaces", boundary),
                    ],
                ),
                ("hidden", vec![hidden]),
            ],
            vec![vec![BridgeGlobSegment::Literal("api".to_owned())]],
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("all live newtype surfaces");
        let surfaces = prepared
            .site(&export_site("api", "surfaces"))
            .expect("surface witness");

        assert!(matches!(
            nominal_declaration(surfaces, "hidden", "Unexposed"),
            BoundaryNominalDeclaration::Newtype {
                surface: BoundaryNewtypeSurface::Unexposed,
                ..
            }
        ));
        assert!(matches!(
            nominal_declaration(surfaces, "api", "Opaque"),
            BoundaryNominalDeclaration::Newtype {
                surface: BoundaryNewtypeSurface::Opaque,
                ..
            }
        ));
        assert!(matches!(
            nominal_declaration(surfaces, "api", "Constructor"),
            BoundaryNominalDeclaration::Newtype {
                surface: BoundaryNewtypeSurface::Constructor { member },
                ..
            } if member == "wrap"
        ));
        assert!(matches!(
            nominal_declaration(surfaces, "api", "Projector"),
            BoundaryNominalDeclaration::Newtype {
                surface: BoundaryNewtypeSurface::Projector { member },
                ..
            } if member == "unwrap"
        ));
        assert!(matches!(
            nominal_declaration(surfaces, "api", "Both"),
            BoundaryNominalDeclaration::Newtype {
                surface: BoundaryNewtypeSurface::Both {
                    constructor,
                    projector,
                },
                ..
            } if constructor == "wrap" && projector == "unwrap"
        ));
        for (module, name) in [
            ("hidden", "Unexposed"),
            ("api", "Opaque"),
            ("api", "Constructor"),
            ("api", "Projector"),
        ] {
            let BoundaryNominalDeclaration::Newtype {
                surface,
                transparent_payload,
                ..
            } = nominal_declaration(surfaces, module, name)
            else {
                unreachable!("the witness names a newtype")
            };
            assert!(surface.uses_nominal_carrier());
            assert!(transparent_payload.is_none());
        }
        let BoundaryNominalDeclaration::Newtype {
            surface,
            transparent_payload,
            ..
        } = nominal_declaration(surfaces, "api", "Both")
        else {
            unreachable!("the witness names a newtype")
        };
        assert!(!surface.uses_nominal_carrier());
        assert!(transparent_payload.is_some());

        let inventory = prepared
            .public_newtypes()
            .map(|entry| (entry.name().clone(), entry.surface().clone()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            inventory.keys().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                semantic_name("api", "Opaque"),
                semantic_name("api", "Constructor"),
                semantic_name("api", "Projector"),
                semantic_name("api", "Both"),
            ])
        );
        assert!(!inventory.contains_key(&semantic_name("hidden", "Unexposed")));
        assert!(matches!(
            inventory.get(&semantic_name("api", "Opaque")),
            Some(BoundaryNewtypeSurface::Opaque)
        ));
        assert!(matches!(
            inventory.get(&semantic_name("api", "Constructor")),
            Some(BoundaryNewtypeSurface::Constructor { member }) if member == "wrap"
        ));
        assert!(matches!(
            inventory.get(&semantic_name("api", "Projector")),
            Some(BoundaryNewtypeSurface::Projector { member }) if member == "unwrap"
        ));
        assert!(matches!(
            inventory.get(&semantic_name("api", "Both")),
            Some(BoundaryNewtypeSurface::Both {
                constructor,
                projector,
            }) if constructor == "wrap" && projector == "unwrap"
        ));

        for (name, expected) in [
            ("Opaque", 0),
            ("Constructor", 1),
            ("Projector", 1),
            ("Both", 2),
        ] {
            let actual = prepared
                .sites()
                .filter(|site| match site.site().owner() {
                    BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, .. }
                    | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, .. } => newtype == name,
                    _ => false,
                })
                .count();
            assert_eq!(actual, expected, "wrong member-site count for {name}");
        }
    }

    #[test]
    fn unrelated_same_leaf_public_newtype_only_grows_the_inventory() {
        let api_items = || {
            vec![
                public_newtype("Token", nominal("host", "Value")),
                nullary_export(
                    "existing",
                    product(nominal("api", "Token"), nominal("host", "Value")),
                ),
            ]
        };
        let bridge = || vec![vec![BridgeGlobSegment::DoubleStar]];
        let before_package = package_with_modules_and_bridge(
            vec![
                ("api", api_items()),
                ("host", vec![roleless_host_type("Value")]),
            ],
            bridge(),
        );
        let after_package = package_with_modules_and_bridge(
            vec![
                ("api", api_items()),
                (
                    "noise",
                    vec![newtype_with_visibility(
                        "Token",
                        nominal("host", "Value"),
                        Visibility::Public,
                        Visibility::Private,
                        Visibility::Private,
                    )],
                ),
                ("host", vec![roleless_host_type("Value")]),
            ],
            bridge(),
        );
        let before = PreparedBoundaryCallableSites::collect_live(&before_package)
            .expect("baseline public inventory");
        let after = PreparedBoundaryCallableSites::collect_live(&after_package)
            .expect("extended public inventory");
        let before_site = before
            .site(&export_site("api", "existing"))
            .expect("baseline site");
        let after_site = after
            .site(&export_site("api", "existing"))
            .expect("same site after unrelated declaration");

        assert_eq!(before_site.plan(), after_site.plan());
        assert_eq!(before_site.nominals(), after_site.nominals());
        assert_eq!(
            before.public_newtypes().len() + 1,
            after.public_newtypes().len()
        );
        assert!(
            after
                .public_newtypes()
                .any(|entry| entry.name() == &semantic_name("noise", "Token"))
        );
    }

    #[test]
    fn live_nominal_projection_fails_for_missing_or_alias_plan_leaves() {
        let missing_package = package_with_items(
            "api",
            vec![nullary_export("missing", nominal("absent", "Type"))],
        );
        assert!(matches!(
            PreparedBoundaryCallableSites::collect_live(&missing_package),
            Err(BoundaryFacadeCollectionError::MissingNominalDependency { name, .. })
                if name == semantic_name("absent", "Type")
        ));

        let alias_package =
            package_with_items("api", vec![public_alias("Alias", Vec::new(), unit())]);
        let alias_scheme = function(unit(), nominal("api", "Alias"), 0);
        let alias_plan = BoundaryCallablePlan::from_authoritative_scheme(
            &alias_scheme,
            vec![BoundaryCallableHeadStageKind::Value],
            &BTreeSet::new(),
        )
        .expect("synthetic plan with a surviving alias leaf");
        let alias_nominal_declarations = live_nominal_declaration_index(&alias_package);
        assert!(matches!(
            live_nominal_dependencies(
                &alias_package,
                &alias_nominal_declarations,
                &live_semantic_nominals(&alias_package),
                &export_site("api", "synthetic"),
                &alias_plan,
            ),
            Err(BoundaryFacadeCollectionError::UnexpectedAliasDependency { name, .. })
                if name == semantic_name("api", "Alias")
        ));
    }

    #[test]
    fn collect_package_adds_retained_host_fn_and_retained_only_shells_without_execution() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type A;
        host type B;
        host fn retired() -> ((A & B) | A);
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        A;
        B;
        retired;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("retained package sites");
        let retired = prepared
            .site(&host_site("api", "retired"))
            .expect("retained host site");

        assert!(retired.execution().is_none());
        assert_eq!(
            retired
                .retained()
                .expect("retained metadata")
                .removed_at_version(),
            2
        );
        assert!(matches!(
            retired.origin(),
            PreparedBoundaryCallableOriginRef::Retained(_)
        ));
        let entry = retired.plan().entry();
        assert!(matches!(
            retired.plan().facade().use_at(entry.returned),
            FacadeUse::Sum { .. }
        ));
        assert_eq!(
            prepared
                .shells()
                .map(FacadeShellId::kind)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([FacadeKind::Product, FacadeKind::Sum])
        );
        assert!(prepared.shell_origins().all(|(_, origin)| matches!(
            origin,
            BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: 2
            }
        )));
        assert!(prepared.public_newtypes().next().is_none());
    }

    #[test]
    fn root_host_bindings_keep_exact_live_and_retained_declarations() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module legacy {
        host type Removed[A];
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module legacy {
        Removed;
      }
    }
  }
}
"#,
        );
        let package = package_with_modules(vec![
            (
                "a",
                vec![
                    host_type("Count", Vec::new(), Some(Role::I32)),
                    host_type("OtherCount", Vec::new(), Some(Role::I32)),
                ],
            ),
            (
                "z",
                vec![host_type(
                    "Box",
                    vec![TypeParam {
                        name: "A".to_owned(),
                        span: span(),
                        kind: Some(Kind::Star),
                    }],
                    None,
                )],
            ),
        ]);
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("live plus retained root host bindings");

        let bindings = prepared.host_bindings().collect::<Vec<_>>();
        assert_eq!(bindings.len(), 4);
        assert_eq!(bindings[0].name(), &semantic_name("a", "Count"));
        assert_eq!(bindings[1].name(), &semantic_name("a", "OtherCount"));
        assert_ne!(bindings[0].name(), bindings[1].name());
        assert!(matches!(
            bindings[0].binding(),
            BoundaryHostTypeBinding::Role(Role::I32)
        ));
        assert!(matches!(
            bindings[1].binding(),
            BoundaryHostTypeBinding::Role(Role::I32)
        ));
        assert!(matches!(
            bindings[0].origin(),
            BoundaryHostBindingOrigin::Live
        ));
        assert_eq!(bindings[2].name(), &semantic_name("legacy", "Removed"));
        assert!(matches!(
            bindings[2].origin(),
            BoundaryHostBindingOrigin::Retained {
                removed_at_version: 2
            }
        ));
        assert_eq!(bindings[2].type_params().len(), 1);
        assert_eq!(bindings[2].type_params()[0].kind(), &Kind::Star);
        assert_eq!(bindings[3].name(), &semantic_name("z", "Box"));
        assert_eq!(bindings[3].type_params().len(), 1);
        assert!(matches!(
            bindings[3].binding(),
            BoundaryHostTypeBinding::Roleless
        ));
    }

    #[test]
    fn root_host_binding_preserves_live_owned_annotation() {
        let mut owned_string = host_type("String", Vec::new(), Some(Role::Str));
        let Item::HostType(declaration) = &mut owned_string else {
            unreachable!("the helper constructs a host type")
        };
        declaration.owned = true;
        let package = package_with_items("api", vec![owned_string]);

        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("live exact host binding");
        let bindings = prepared.host_bindings().collect::<Vec<_>>();
        let [binding] = bindings.as_slice() else {
            panic!("one live exact host binding: {bindings:#?}")
        };
        assert_eq!(binding.name(), &semantic_name("api", "String"));
        assert!(binding.owned());
        assert!(matches!(binding.origin(), BoundaryHostBindingOrigin::Live));
    }

    #[test]
    fn root_host_bindings_prefer_incompatible_live_over_retained_snapshot() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type H role(str);
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        H;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", vec![host_type("H", Vec::new(), Some(Role::I32))]);

        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("the current exact host binding dominates incompatible history");
        let binding = prepared
            .host_bindings()
            .find(|binding| binding.name() == &semantic_name("api", "H"))
            .expect("current exact host binding");
        assert!(matches!(binding.origin(), BoundaryHostBindingOrigin::Live));
        assert!(matches!(
            binding.binding(),
            BoundaryHostTypeBinding::Role(Role::I32)
        ));
    }

    #[test]
    fn root_host_bindings_omit_incompatible_retained_snapshots() {
        let mut bindings = BTreeMap::new();
        let mut suppressed = BTreeSet::new();
        let retained = |role, removed_at_version| BoundaryHostBinding {
            name: semantic_name("api", "H"),
            type_params: Vec::new(),
            binding: BoundaryHostTypeBinding::Role(role),
            owned: false,
            origin: BoundaryHostBindingOrigin::Retained { removed_at_version },
        };
        insert_host_binding(&mut bindings, &mut suppressed, retained(Role::Str, 2))
            .expect("first retained snapshot");
        insert_host_binding(&mut bindings, &mut suppressed, retained(Role::I32, 4))
            .expect("incompatible optional history is omitted");
        assert!(bindings.is_empty());
        assert_eq!(suppressed, BTreeSet::from([semantic_name("api", "H")]));
    }

    #[test]
    fn retained_host_fn_closure_keeps_its_exact_host_binding() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module legacy {
        host type H;
        host fn old(value: H) -> H;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module legacy {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("legacy", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("retained host-function type closure");

        let bindings = prepared.host_bindings().collect::<Vec<_>>();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].name(), &semantic_name("legacy", "H"));
        assert!(matches!(
            bindings[0].binding(),
            BoundaryHostTypeBinding::Roleless
        ));
        assert!(matches!(
            bindings[0].origin(),
            BoundaryHostBindingOrigin::Retained {
                removed_at_version: 2
            }
        ));
    }

    #[test]
    fn root_host_binding_plan_is_open_world_under_unrelated_declarations() {
        let baseline = package_with_items(
            "api",
            vec![
                host_type("Count", Vec::new(), Some(Role::I32)),
                host_type(
                    "Box",
                    vec![TypeParam {
                        name: "A".to_owned(),
                        span: span(),
                        kind: Some(Kind::Star),
                    }],
                    None,
                ),
            ],
        );
        let expanded = package_with_items(
            "api",
            vec![
                host_type("Count", Vec::new(), Some(Role::I32)),
                host_type(
                    "Box",
                    vec![TypeParam {
                        name: "A".to_owned(),
                        span: span(),
                        kind: Some(Kind::Star),
                    }],
                    None,
                ),
                nullary_export("unrelated", unit()),
            ],
        );

        let baseline = PreparedBoundaryCallableSites::collect_live(&baseline)
            .expect("baseline binding plan")
            .host_bindings()
            .cloned()
            .collect::<Vec<_>>();
        let expanded = PreparedBoundaryCallableSites::collect_live(&expanded)
            .expect("expanded binding plan")
            .host_bindings()
            .cloned()
            .collect::<Vec<_>>();

        assert_eq!(baseline, expanded);
    }

    #[test]
    fn retained_nominal_projection_preserves_roles_kinds_and_newtype_closure() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Count role(i32);
        host type Token;
        host type Box[A];
        newtype Inner : Token | . { pub constructor make_inner; pub projector read_inner; };
        newtype Wrapper[A] <Hidden> : (A & Inner & Hidden) {
          pub constructor wrap;
          pub projector unwrap;
        };
        host fn old(value: (Count & Token & Box(.) & Wrapper(.))) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Count;
        Token;
        Box;
        Inner;
        Wrapper;
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("retained declaration-local nominal closure");
        let old = prepared
            .site(&host_site("api", "old"))
            .expect("retained host root");
        assert_plan_nominals_are_site_local(old);
        assert!(old.execution().is_none());

        assert!(matches!(
            nominal_declaration(old, "api", "Count"),
            BoundaryNominalDeclaration::HostType {
                type_params,
                binding: BoundaryHostTypeBinding::Role(Role::I32),
            } if type_params.is_empty()
        ));
        assert!(matches!(
            nominal_declaration(old, "api", "Token"),
            BoundaryNominalDeclaration::HostType {
                type_params,
                binding: BoundaryHostTypeBinding::Roleless,
            } if type_params.is_empty()
        ));
        assert!(matches!(
            nominal_declaration(old, "api", "Box"),
            BoundaryNominalDeclaration::HostType {
                type_params,
                binding: BoundaryHostTypeBinding::Roleless,
            } if type_params.len() == 1
                && type_params[0].name() == "A"
                && type_params[0].kind() == &Kind::Star
        ));
        assert!(matches!(
            nominal_declaration(old, "api", "Wrapper"),
            BoundaryNominalDeclaration::Newtype {
                type_params,
                existential_params,
                transparent_payload: Some(_),
                surface: BoundaryNewtypeSurface::Both {
                    constructor,
                    projector,
                },
                ..
            } if type_params.len() == 1
                && type_params[0].kind() == &Kind::Star
                && existential_params.len() == 1
                && existential_params[0].name() == "Hidden"
                && constructor == "wrap"
                && projector == "unwrap"
        ));
        assert!(matches!(
            nominal_declaration(old, "api", "Inner"),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(_),
                surface: BoundaryNewtypeSurface::Both { .. },
                ..
            }
        ));
        assert_eq!(
            newtype_payload(old, "api", "Inner")
                .shell_dependencies()
                .map(FacadeShellId::kind)
                .collect::<Vec<_>>(),
            vec![FacadeKind::Sum]
        );
        assert!(
            prepared
                .shells()
                .all(|shell| shell.kind() != FacadeKind::Sum)
        );
        assert_eq!(old.nominals().declarations().len(), 5);
    }

    #[test]
    fn retained_host_signature_keeps_its_frozen_nominal_carrier_inventory() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Token[A] : A { pub constructor make_token; projector read_token; };
        host fn old(value: Token(.)) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Token;
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("retained carrier closure");

        assert!(prepared.public_newtypes().next().is_none());
        let carriers = prepared.retained_public_newtypes().collect::<Vec<_>>();
        let [carrier] = carriers.as_slice() else {
            panic!("one frozen nominal carrier: {carriers:#?}")
        };
        assert_eq!(carrier.name(), &semantic_name("api", "Token"));
        assert_eq!(carrier.type_params().len(), 1);
        assert_eq!(carrier.type_params()[0].kind(), &Kind::Star);
        assert!(carrier.existential_params().is_empty());
        assert!(matches!(
            carrier.surface(),
            BoundaryNewtypeSurface::Constructor { member } if member == "make_token"
        ));
        assert_eq!(
            prepared.retained_public_newtype_removed_at(carrier.name()),
            Some(2)
        );
        assert!(prepared.site(&host_site("api", "old")).is_some());
    }

    #[test]
    fn retained_host_signature_inventories_transparent_newtype_constructor_witness() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Str role(str);
        newtype Box[A] : A { pub constructor make_box; pub projector un_box; };
        newtype Lift[*F] : F(Str) { pub constructor make_lift; pub projector un_lift; };
        host fn old(value: Lift(Box)) -> Lift(Box);
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Box;
        Lift;
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("retained transparent constructor inventory");

        let carrier = prepared
            .retained_public_newtypes()
            .find(|entry| entry.name() == &semantic_name("api", "Box"))
            .expect("frozen transparent Box constructor witness");
        assert_eq!(carrier.name(), &semantic_name("api", "Box"));
        assert!(matches!(
            carrier.surface(),
            BoundaryNewtypeSurface::Both { constructor, projector }
                if constructor == "make_box" && projector == "un_box"
        ));
        assert_eq!(
            prepared.retained_public_newtype_removed_at(carrier.name()),
            Some(2)
        );
    }

    #[test]
    fn retained_projection_fails_when_a_transitive_frozen_leaf_is_missing() {
        let mut replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Raw;
        newtype Wrapper : Raw { pub constructor wrap; pub projector unwrap; };
        host fn old(value: Wrapper) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Raw;
        Wrapper;
        old;
      }
    }
  }
}
"#,
        );
        removed_host_mut(&mut replayed, "api", "old")
            .frozen_type_closure
            .as_mut()
            .expect("old root closure")
            .declarations
            .remove(&QualifiedName::new("api", "Raw"));
        let package = package_with_items("api", Vec::new());
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&replayed)),
            Err(BoundaryFacadeCollectionError::UnresolvedRetainedType { path, .. })
                if path == vec!["Raw".to_owned()]
        ));
    }

    #[test]
    fn retained_carrier_does_not_require_or_publish_its_hidden_payload_closure() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Secret;
        newtype Carrier : (Secret & .) { constructor make; projector read; };
        host fn old(value: Carrier) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Secret;
        Carrier;
        old;
      }
    }
  }
}
"#,
        );
        let closure = removed_host(&replayed, "api", "old")
            .frozen_type_closure
            .as_ref()
            .expect("old root closure");
        assert!(
            closure
                .declarations
                .contains_key(&QualifiedName::new("api", "Carrier"))
        );
        assert!(
            !closure
                .declarations
                .contains_key(&QualifiedName::new("api", "Secret"))
        );

        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("opaque carrier needs no hidden payload closure");
        let old = prepared
            .site(&host_site("api", "old"))
            .expect("retained carrier root");
        assert_plan_nominals_are_site_local(old);
        assert!(old.execution().is_none());
        assert!(matches!(
            nominal_declaration(old, "api", "Carrier"),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: None,
                surface: BoundaryNewtypeSurface::Opaque,
                ..
            }
        ));
        assert_eq!(old.nominals().declarations().len(), 1);
        assert!(prepared.shells().next().is_none());
    }

    #[test]
    fn retained_host_and_live_export_same_module_leaf_coexist() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn same() -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      module api {
        pub fn same() -> .;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", vec![nullary_export("same", unit())]);
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("role-distinct side-flip sites");
        let live = prepared
            .site(&export_site("api", "same"))
            .expect("live export");
        let retained = prepared
            .site(&host_site("api", "same"))
            .expect("retained host requirement");

        assert!(matches!(
            live.origin(),
            PreparedBoundaryCallableOriginRef::Live(_)
        ));
        assert!(live.execution().is_some());
        assert!(retained.execution().is_none());
        assert_eq!(
            retained
                .retained()
                .expect("retained metadata")
                .removed_at_version(),
            2
        );
        assert_eq!(prepared.sites().len(), 2);
    }

    #[test]
    fn live_host_dominates_retained_history_at_the_same_exact_site() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn same(value: .) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        same;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", vec![host_function("same", unit(), unit())]);
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("the current exact site dominates its removed-history snapshot");
        let site = prepared
            .site(&host_site("api", "same"))
            .expect("the live site remains present");
        assert!(matches!(
            site.origin(),
            PreparedBoundaryCallableOriginRef::Live(_)
        ));
        assert!(site.execution().is_some());
        assert!(site.retained().is_none());
    }

    #[test]
    fn duplicate_retained_sites_fail_closed() {
        let mut replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old() -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let duplicate = removed_host(&replayed, "api", "old").clone();
        replayed.removed.push(duplicate);
        let package = package_with_items("api", Vec::new());
        let error = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect_err("duplicate retained roots must fail closed");
        let BoundaryFacadeCollectionError::Duplicate(conflict) = error else {
            panic!("expected duplicate-site error");
        };
        assert_eq!(conflict.site(), &host_site("api", "old"));
    }

    #[test]
    fn inconsistent_retained_host_records_fail_closed() {
        let original = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Payload;
        host fn old(value: Payload) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Payload;
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());

        let mut missing_closure = original.clone();
        removed_host_mut(&mut missing_closure, "api", "old").frozen_type_closure = None;
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&missing_closure)),
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. })
        ));

        let mut mismatched_name = original.clone();
        let SigItem::HostFn(host) =
            &mut removed_host_mut(&mut mismatched_name, "api", "old").frozen
        else {
            panic!("fixture freezes a host fn");
        };
        host.name = "different".to_owned();
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&mismatched_name)),
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. })
        ));

        let mut wrong_side = original.clone();
        removed_host_mut(&mut wrong_side, "api", "old").entry.side = ContractSide::Export;
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&wrong_side)),
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. })
        ));

        let mut mismatched_signature = original.clone();
        let ContractKind::Fn { signature, .. } =
            &mut removed_host_mut(&mut mismatched_signature, "api", "old")
                .entry
                .kind
        else {
            panic!("fixture records a function contract");
        };
        signature.push_str(" mutated");
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&mismatched_signature)),
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. })
        ));

        let mut pure_host = original.clone();
        let ContractKind::Fn { pure, .. } =
            &mut removed_host_mut(&mut pure_host, "api", "old").entry.kind
        else {
            panic!("fixture records a function contract");
        };
        *pure = true;
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&pure_host)),
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. })
        ));

        let mut malformed_groups = original.clone();
        let SigItem::HostFn(host) =
            &mut removed_host_mut(&mut malformed_groups, "api", "old").frozen
        else {
            panic!("fixture freezes a host fn");
        };
        host.param_groups = vec![crate::ast::SignatureGroupKind::Value { len: usize::MAX }];
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&malformed_groups)),
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. })
        ));

        let frozen_type = original
            .removed
            .iter()
            .find(|item| item.entry.name == QualifiedName::new("api", "Payload"))
            .expect("removed frozen host type")
            .frozen
            .clone();
        let mut wrong_kind = original;
        removed_host_mut(&mut wrong_kind, "api", "old").frozen = frozen_type;
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&wrong_kind)),
            Err(BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. })
        ));
    }

    #[test]
    fn retained_alias_epoch_does_not_bind_live_same_name_newtype() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module old {
        newtype Nominal : . { pub constructor wrap; pub projector unwrap; };
      };
      module api {
        import old(Nominal);
        type Wrapped = Nominal;
        host fn consume(value: Wrapped) -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      module api {
        newtype Wrapped : . { pub constructor make; pub projector read; };
      }
    };
    remove {
      module api {
        consume;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", vec![public_newtype("Wrapped", unit())]);
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("historical alias root");
        let retained = prepared
            .site(&host_site("api", "consume"))
            .expect("retained host root");
        assert_plan_nominals_are_site_local(retained);
        assert!(retained.execution().is_none());
        let BoundaryCallableHeadStage::Value { slots } = retained.plan().entry().head_stages[0]
        else {
            panic!("retained consume has one value head");
        };
        assert!(matches!(
            retained.plan().facade().use_at(slots[0]),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("old", "Nominal")
        ));
        assert!(retained.plan().facade().uses().iter().all(|use_| !matches!(
            use_,
            FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "Wrapped")
        )));
        assert!(matches!(
            nominal_declaration(retained, "old", "Nominal"),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(_),
                surface: BoundaryNewtypeSurface::Both { .. },
                ..
            }
        ));
        assert!(
            retained
                .nominals()
                .declaration(&semantic_name("api", "Wrapped"))
                .is_none()
        );
    }

    #[test]
    fn retained_newtype_epoch_does_not_unfold_live_same_name_alias() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Raw;
        newtype Wrapped : Raw { pub constructor wrap; pub projector unwrap; };
        host fn consume(value: Wrapped) -> (Wrapped & Raw);
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      module api {
        type Wrapped = (. & .);
      }
    };
    remove {
      module api {
        consume;
      }
    }
  }
}
"#,
        );
        let package = package_with_items(
            "api",
            vec![public_alias("Wrapped", Vec::new(), product(unit(), unit()))],
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("historical nominal root");
        let retained = prepared
            .site(&host_site("api", "consume"))
            .expect("retained host root");
        assert_plan_nominals_are_site_local(retained);
        let BoundaryCallableHeadStage::Value { slots } = retained.plan().entry().head_stages[0]
        else {
            panic!("retained consume has one value head");
        };
        assert!(matches!(
            retained.plan().facade().use_at(slots[0]),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "Wrapped")
        ));
        let FacadeUse::Product { shell, .. } = retained
            .plan()
            .facade()
            .use_at(retained.plan().entry().returned)
        else {
            panic!("retained return is a product facade");
        };
        assert_eq!(
            shell.ordered_keys(),
            &[
                SemanticKey::Bare {
                    name: "Wrapped".to_owned(),
                },
                SemanticKey::Positional { index: 1 },
            ]
        );
        assert!(matches!(
            nominal_declaration(retained, "api", "Wrapped"),
            BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(_),
                surface: BoundaryNewtypeSurface::Both { .. },
                ..
            }
        ));
        assert!(matches!(
            nominal_declaration(retained, "api", "Raw"),
            BoundaryNominalDeclaration::HostType {
                binding: BoundaryHostTypeBinding::Roleless,
                ..
            }
        ));
    }

    #[test]
    fn retained_roots_omit_incompatible_same_name_newtype_epochs() {
        let replayed = replayed_interface(
            r#"signature fixture v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Epoch : . { pub constructor make; pub projector read; };
        host fn first(value: Epoch) -> .;
      }
    }
  }
}
v(2) {
  breaking {
    add {
      module api {
        host fn second(value: Epoch) -> .;
      }
    };
    modify {
      module api {
        newtype Epoch : (. & .) { pub constructor make; pub projector read; };
      }
    };
    remove {
      module api {
        first;
      }
    }
  }
}
v(3) {
  breaking {
    remove {
      module api {
        Epoch;
        second;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("incompatible optional retained epochs are omitted");
        assert!(prepared.site(&host_site("api", "first")).is_none());
        assert!(prepared.site(&host_site("api", "second")).is_none());
        assert!(prepared.retained_public_newtypes().next().is_none());
        assert!(prepared.shells().next().is_none());
    }

    #[test]
    fn retained_root_keeps_semantically_equal_live_newtype_despite_source_spans() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Box[A] : A { pub constructor make; pub projector read; };
        host fn old(value: Box(.)) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items(
            "api",
            vec![Item::Newtype(Newtype {
                vis: Visibility::Public,
                rec_span: None,
                name: "Box".to_owned(),
                name_span: span(),
                type_params: vec![TypeParam {
                    name: "B".to_owned(),
                    span: span(),
                    kind: Some(Kind::Star),
                }],
                existential_params: Vec::new(),
                payload: bare("B"),
                constructor: member("make", Visibility::Public),
                projector: member("read", Visibility::Public),
                meta: meta(),
                editable_span: None,
                doc: None,
            })],
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("equal live and retained nominal semantics");
        assert!(prepared.site(&host_site("api", "old")).is_some());
    }

    #[test]
    fn retained_roots_keep_alpha_renamed_newtype_epochs() {
        let replayed = replayed_interface(
            r#"signature fixture v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Box[A] : A { pub constructor wrap; pub projector unwrap; };
        host fn first(value: Box(.)) -> .;
      }
    }
  }
}
v(2) {
  breaking {
    add { module api { host fn second(value: Box(.)) -> .; } };
    modify {
      module api {
        newtype Box[B] : B { pub constructor wrap; pub projector unwrap; };
      }
    };
    remove { module api { first; } }
  }
}
v(3) {
  breaking { remove { module api { Box; second; } } }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("alpha-renamed retained epochs share one semantic carrier");
        assert!(prepared.site(&host_site("api", "first")).is_some());
        assert!(prepared.site(&host_site("api", "second")).is_some());
        assert_eq!(prepared.retained_public_newtypes().count(), 1);
    }

    #[test]
    fn retained_root_omits_incompatible_live_opaque_newtype_arity() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Token[A] : A { constructor make; projector read; };
        host fn old(value: Token(.)) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items(
            "api",
            vec![Item::Newtype(Newtype {
                vis: Visibility::Public,
                rec_span: None,
                name: "Token".to_owned(),
                name_span: span(),
                type_params: vec![
                    TypeParam {
                        name: "A".to_owned(),
                        span: span(),
                        kind: Some(Kind::Star),
                    },
                    TypeParam {
                        name: "B".to_owned(),
                        span: span(),
                        kind: Some(Kind::Star),
                    },
                ],
                existential_params: Vec::new(),
                payload: product(bare("A"), bare("B")),
                constructor: member("make", Visibility::Private),
                projector: member("read", Visibility::Private),
                meta: meta(),
                editable_span: None,
                doc: None,
            })],
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("incompatible optional opaque history is omitted");
        assert!(prepared.site(&host_site("api", "old")).is_none());
    }

    #[test]
    fn retained_root_keeps_live_compatible_epoch_while_omitting_older_conflict() {
        let replayed = replayed_interface(
            r#"signature fixture v(4);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Str role(str);
        newtype Token : . { pub constructor wrap; pub projector unwrap; };
        host fn a_old(value: Token) -> .;
      }
    }
  }
}
v(2) {
  breaking {
    modify {
      module api {
        newtype Token : Str { pub constructor wrap; pub projector unwrap; };
      }
    };
    remove {
      module api {
        a_old;
      }
    }
  }
}
v(3) {
  breaking {
    add { module api { host fn z_recent(value: Token) -> .; } }
  }
}
v(4) {
  nonbreaking { remove { module api { z_recent; } } }
}
"#,
        );
        let package = package_with_items(
            "api",
            vec![
                host_type("Str", Vec::new(), Some(Role::Str)),
                public_newtype("Token", nominal("api", "Str")),
                host_function("current", nominal("api", "Token"), nominal("api", "Token")),
            ],
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("live-compatible retained epoch survives");
        assert!(prepared.site(&host_site("api", "a_old")).is_none());
        assert!(prepared.site(&host_site("api", "z_recent")).is_some());
    }

    #[test]
    fn retained_root_keeps_live_compatible_host_type_epoch() {
        let replayed = replayed_interface(
            r#"signature fixture v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type H role(str);
        host fn a_old(value: H) -> .;
      }
    }
  }
}
v(2) {
  breaking {
    add { module api { host fn z_recent(value: H) -> .; } };
    modify { module api { host type H role(i32); } };
    remove { module api { a_old; } }
  }
}
v(3) {
  nonbreaking { remove { module api { z_recent; } } }
}
"#,
        );
        let package = package_with_items("api", vec![host_type("H", Vec::new(), Some(Role::I32))]);
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("live-compatible retained host-type epoch survives");
        assert!(prepared.site(&host_site("api", "a_old")).is_none());
        assert!(prepared.site(&host_site("api", "z_recent")).is_some());
    }

    #[test]
    fn retained_root_ignores_rust_only_live_host_ownership() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Text role(str);
        host fn old(value: Text) -> Text;
      }
    }
  }
}
v(2) { nonbreaking { remove { module api { old; } } } }
"#,
        );
        let mut text = host_type("Text", Vec::new(), Some(Role::Str));
        let Item::HostType(text) = &mut text else {
            unreachable!("the fixture helper constructs a host type")
        };
        text.owned = true;
        let package = package_with_items("api", vec![Item::HostType(text.clone())]);
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("the redundant Rust source annotation is not an epoch conflict");
        assert!(prepared.site(&host_site("api", "old")).is_some());
        let binding = prepared
            .host_bindings()
            .find(|binding| binding.name() == &semantic_name("api", "Text"))
            .expect("live Text binding");
        assert!(binding.owned());
        assert!(matches!(binding.origin(), BoundaryHostBindingOrigin::Live));
    }

    #[test]
    fn retained_alias_body_uses_its_own_frozen_imports() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module types {
        host type Payload;
      };
      module defs {
        import types as t;
        type Wrapped = t.Payload;
      };
      module api {
        import defs(Wrapped);
        host fn consume(value: Wrapped) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        consume;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("declaration-local frozen imports");
        let retained = prepared
            .site(&host_site("api", "consume"))
            .expect("retained host root");
        let BoundaryCallableHeadStage::Value { slots } = retained.plan().entry().head_stages[0]
        else {
            panic!("retained consume has one value head");
        };
        assert!(matches!(
            retained.plan().facade().use_at(slots[0]),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("types", "Payload")
        ));
    }

    #[test]
    fn retained_generic_alias_substitution_preserves_the_root_binder() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Box[A];
        type Wrapped[A] = Box(A);
        host fn consume[A](value: Wrapped(A)) -> Wrapped(A);
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        consume;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("generic alias retained root");
        let retained = prepared
            .site(&host_site("api", "consume"))
            .expect("retained consume root");
        let entry = retained.plan().entry();
        let BoundaryCallableHeadStage::Type { id: binder, .. } = entry.head_stages[0] else {
            panic!("first retained head is the type binder");
        };
        let BoundaryCallableHeadStage::Value { slots } = entry.head_stages[1] else {
            panic!("second retained head is the value group");
        };
        for use_id in [slots[0], entry.returned] {
            let FacadeUse::Apply {
                constructor, args, ..
            } = retained.plan().facade().use_at(use_id)
            else {
                panic!("expanded alias is an application of the frozen host type");
            };
            assert!(matches!(
                retained.plan().facade().use_at(*constructor),
                FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "Box")
            ));
            assert!(matches!(
                retained.plan().facade().use_at(args[0]),
                FacadeUse::Bound { binder: actual, .. } if actual == &binder
            ));
        }

        let mut unsaturated = replayed.clone();
        let SigItem::HostFn(host) =
            &mut removed_host_mut(&mut unsaturated, "api", "consume").frozen
        else {
            panic!("fixture freezes a host fn");
        };
        let crate::ast::HostFnParam::Value(value) = &mut host.params[1] else {
            panic!("second parameter is the value group");
        };
        let Type::Path { args, .. } = &mut value.ty else {
            panic!("value parameter uses the generic alias");
        };
        args.clear();
        refresh_removed_host_contract_signature(&mut unsaturated, "api", "consume");
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&unsaturated)),
            Err(BoundaryFacadeCollectionError::RetainedAliasArity {
                expected: 1,
                actual: 0,
                ..
            })
        ));
    }

    #[test]
    fn retained_higher_kinded_alias_accepts_a_partial_nominal_application() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Pair[A][B];
        type Apply[*F] = F(.);
        host fn old(value: Apply(Pair(.))) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("kind-valid retained higher-kinded alias");
        let retained = prepared
            .site(&host_site("api", "old"))
            .expect("retained higher-kinded root");
        let BoundaryCallableHeadStage::Value { slots } = retained.plan().entry().head_stages[0]
        else {
            panic!("retained higher-kinded root has one value head");
        };
        let FacadeUse::Apply {
            constructor, args, ..
        } = retained.plan().facade().use_at(slots[0])
        else {
            panic!("partial Pair applied through the alias must be saturated");
        };
        assert!(matches!(
            retained.plan().facade().use_at(*constructor),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "Pair")
        ));
        assert_eq!(args.len(), 2);
        assert!(args.iter().all(|argument| matches!(
            retained.plan().facade().use_at(*argument),
            FacadeUse::Unit { .. }
        )));
    }

    #[test]
    fn retained_callable_spine_accepts_a_trailing_higher_kinded_stage() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old()[*F] -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("valid trailing higher-kinded declaration stage");
        let retained = prepared
            .site(&host_site("api", "old"))
            .expect("retained trailing-stage root");
        assert_eq!(
            retained.plan().head_stage_kinds(),
            &[
                BoundaryCallableHeadStageKind::Value,
                BoundaryCallableHeadStageKind::Type,
            ]
        );
        assert!(retained.execution().is_none());
    }

    #[test]
    fn retained_value_type_rejects_an_incomplete_higher_kinded_forall() {
        let replayed = replayed_invalid_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old(callback: [*F] .) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&replayed)),
            Err(BoundaryFacadeCollectionError::InvalidRetainedKind { .. })
        ));
    }

    #[test]
    fn retained_kind_walk_rejects_a_wrong_kind_binder_application() {
        let replayed = replayed_invalid_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old[*F](value: F(F)) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&replayed)),
            Err(BoundaryFacadeCollectionError::InvalidRetainedKind { .. })
        ));
    }

    #[test]
    fn retained_kind_walk_rejects_a_wrong_kind_alias_actual_without_panicking() {
        let replayed = replayed_invalid_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        type Apply[*F] = F(.);
        host fn old(value: Apply(.)) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&replayed)),
            Err(BoundaryFacadeCollectionError::InvalidRetainedKind { .. })
        ));
    }

    #[test]
    fn retained_kind_walk_rejects_under_and_over_applied_nominals() {
        let cases = [
            (
                "under-applied host type",
                "host type Pair[A][B];",
                "host fn old(value: Pair(.)) -> .;",
            ),
            (
                "over-applied host type",
                "host type Pair[A][B];",
                "host fn old(value: Pair(., ., .)) -> .;",
            ),
            (
                "under-applied newtype",
                "newtype Box[A] : . { pub constructor wrap; pub projector unwrap; };",
                "host fn old(value: Box) -> .;",
            ),
            (
                "over-applied newtype",
                "newtype Box[A] : . { pub constructor wrap; pub projector unwrap; };",
                "host fn old(value: Box(., .)) -> .;",
            ),
        ];
        let package = package_with_items("api", Vec::new());

        for (label, declaration, host) in cases {
            let source = format!(
                "signature fixture v(2);\n\
                 v(1) {{\n  nonbreaking {{\n    add {{\n      module api {{\n        \
                 {declaration}\n        {host}\n      }}\n    }}\n  }}\n}}\n\
                 v(2) {{\n  nonbreaking {{\n    remove {{\n      module api {{\n        \
                 old;\n      }}\n    }}\n  }}\n}}\n"
            );
            let replayed = replayed_invalid_interface(&source);
            let error =
                PreparedBoundaryCallableSites::collect(&package, Some(&replayed)).expect_err(label);
            assert!(
                matches!(
                    error,
                    BoundaryFacadeCollectionError::InvalidRetainedKind { .. }
                ),
                "{label}: {error:?}"
            );
        }
    }

    #[test]
    fn retained_kind_walk_rejects_an_invalid_alias_body() {
        let replayed = replayed_invalid_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        type Bad[A] = A(.);
        host fn old(value: Bad(.)) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        assert!(matches!(
            PreparedBoundaryCallableSites::collect(&package, Some(&replayed)),
            Err(BoundaryFacadeCollectionError::InvalidRetainedKind { .. })
        ));
    }

    #[test]
    fn future_same_module_declaration_does_not_bind_retained_root() {
        let replayed = replayed_invalid_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old(value: Future) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    add {
      module api {
        host type Future;
      }
    };
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let without_live = package_with_items("api", Vec::new());
        let with_live = package_with_items("api", vec![public_newtype("Future", unit())]);
        let first = PreparedBoundaryCallableSites::collect(&without_live, Some(&replayed))
            .expect_err("future declaration is absent from the frozen root");
        let second = PreparedBoundaryCallableSites::collect(&with_live, Some(&replayed))
            .expect_err("live declaration cannot bind an older frozen root");
        assert_eq!(first, second);
        assert!(matches!(
            first,
            BoundaryFacadeCollectionError::UnresolvedRetainedType { .. }
        ));
    }

    #[test]
    fn retained_comptime_type_names_use_their_runtime_erasure() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        import __comptime__;
        host fn old(value: __Type__) -> Comptime_bool;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        assert!(
            removed_host(&replayed, "api", "old")
                .frozen_type_closure
                .as_ref()
                .expect("frozen builtin root")
                .declarations
                .is_empty()
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("builtin runtime erasure");
        let retained = prepared
            .site(&host_site("api", "old"))
            .expect("retained builtin root");
        let entry = retained.plan().entry();
        let BoundaryCallableHeadStage::Value { slots } = entry.head_stages[0] else {
            panic!("retained builtin root has one value head");
        };
        assert!(matches!(
            retained.plan().facade().use_at(slots[0]),
            FacadeUse::Bottom { .. }
        ));
        assert!(matches!(
            retained.plan().facade().use_at(entry.returned),
            FacadeUse::Unit { .. }
        ));
    }

    #[test]
    fn retained_type_binder_shadows_a_comptime_type_name() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old[Comptime_bool](value: Comptime_bool) -> Comptime_bool;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("builtin-spelled lexical binder");
        let retained = prepared
            .site(&host_site("api", "old"))
            .expect("retained binder root");
        let entry = retained.plan().entry();
        let BoundaryCallableHeadStage::Type { id: binder, .. } = entry.head_stages[0] else {
            panic!("first retained head is the lexical binder");
        };
        let BoundaryCallableHeadStage::Value { slots } = entry.head_stages[1] else {
            panic!("second retained head is the value group");
        };
        for use_id in [slots[0], entry.returned] {
            assert!(matches!(
                retained.plan().facade().use_at(use_id),
                FacadeUse::Bound { binder: actual, .. } if actual == &binder
            ));
        }
    }

    #[test]
    fn retained_frozen_nominal_shadows_a_comptime_type_name() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Comptime_bool;
        host fn old(value: Comptime_bool) -> Comptime_bool;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("frozen nominal shadows builtin spelling");
        let retained = prepared
            .site(&host_site("api", "old"))
            .expect("retained nominal root");
        let entry = retained.plan().entry();
        let BoundaryCallableHeadStage::Value { slots } = entry.head_stages[0] else {
            panic!("retained nominal root has one value head");
        };
        for use_id in [slots[0], entry.returned] {
            assert!(matches!(
                retained.plan().facade().use_at(use_id),
                FacadeUse::Nominal { name, .. }
                    if name == &semantic_name("api", "Comptime_bool")
            ));
        }
    }

    #[test]
    fn retained_missing_imports_cannot_fall_back_to_comptime_type_names() {
        let cases = [
            (
                "missing selective import",
                "import missing(Comptime_bool);",
                "Comptime_bool",
            ),
            (
                "missing qualified import",
                "import missing as m;",
                "m.Comptime_bool",
            ),
        ];
        let package = package_with_items("api", Vec::new());

        for (label, usage, written) in cases {
            let source = format!(
                "signature fixture v(2);\n\
                 v(1) {{\n  nonbreaking {{\n    add {{\n      module api {{\n        \
                 {usage}\n        host fn old(value: {written}) -> .;\n      }}\n    }}\n  }}\n\
                 }}\n\
                 v(2) {{\n  nonbreaking {{\n    remove {{\n      module api {{\n        \
                 old;\n      }}\n    }}\n  }}\n}}\n"
            );
            let replayed = replayed_invalid_interface(&source);
            assert!(
                removed_host(&replayed, "api", "old")
                    .frozen_type_closure
                    .as_ref()
                    .expect("missing import root has a frozen closure")
                    .declarations
                    .is_empty()
            );
            let error =
                PreparedBoundaryCallableSites::collect(&package, Some(&replayed)).expect_err(label);
            assert!(
                matches!(
                    error,
                    BoundaryFacadeCollectionError::UnresolvedRetainedType { .. }
                ),
                "{label}: {error:?}"
            );
        }
    }

    #[test]
    fn retained_roots_never_union_frozen_nominals() {
        let mut forward = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Onlyone : . { pub constructor wrap; pub projector unwrap; };
        host fn first(value: Onlyone) -> .;
        host fn second(value: Onlyone) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        first;
        second;
      }
    }
  }
}
"#,
        );
        removed_host_mut(&mut forward, "api", "second")
            .frozen_type_closure
            .as_mut()
            .expect("second frozen root")
            .declarations
            .clear();
        let mut reverse = forward.clone();
        reverse.removed.reverse();
        let package = package_with_items("api", Vec::new());

        let forward_error = PreparedBoundaryCallableSites::collect(&package, Some(&forward))
            .expect_err("second root cannot borrow the first root's nominal");
        let reverse_error = PreparedBoundaryCallableSites::collect(&package, Some(&reverse))
            .expect_err("root-local failure is independent of encounter order");
        assert_eq!(forward_error, reverse_error);
        assert!(matches!(
            forward_error,
            BoundaryFacadeCollectionError::InconsistentRetainedRoot { .. }
        ));
    }

    #[test]
    fn retained_interleaved_head_cut_is_exact_and_returned_callable_is_not_a_head() {
        let replayed = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type R;
        host type X;
        host type Y;
        host fn staged[A](callback: (A & X) -> R)[B](value: B) -> (X) -> Y;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        staged;
      }
    }
  }
}
"#,
        );
        let package = package_with_items("api", Vec::new());
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("interleaved retained root");
        let retained = prepared
            .site(&host_site("api", "staged"))
            .expect("retained staged root");

        assert_eq!(
            retained.plan().head_stage_kinds(),
            &[
                BoundaryCallableHeadStageKind::Type,
                BoundaryCallableHeadStageKind::Value,
                BoundaryCallableHeadStageKind::Type,
                BoundaryCallableHeadStageKind::Value,
            ]
        );
        let entry = retained.plan().entry();
        let BoundaryCallableHeadStage::Value {
            slots: callback_slots,
        } = entry.head_stages[1]
        else {
            panic!("second retained head is the callback value group");
        };
        assert_eq!(callback_slots.len(), 1);
        assert!(matches!(
            retained.plan().facade().use_at(callback_slots[0]),
            FacadeUse::Function { .. }
        ));
        let callback_shell = retained
            .plan()
            .facade()
            .function_shell(callback_slots[0])
            .expect("two-slot retained callback owns one exact shell");
        assert_eq!(callback_shell.ordered_keys().len(), 2);
        assert!(prepared.shells().any(|shell| shell == callback_shell));
        assert!(matches!(
            retained.plan().facade().use_at(entry.returned),
            FacadeUse::Function { .. }
        ));
        let head_shell = retained
            .plan()
            .head_value_shell()
            .expect("two retained head values own one exact shell");
        assert_eq!(head_shell.ordered_keys().len(), 2);
        assert!(prepared.shells().any(|shell| shell == head_shell));
        assert!(retained.execution().is_none());
        let presentation = retained.presentation();
        assert_eq!(presentation.head_stages().len(), 4);
        let Some(CallableExecutionStage::Value(callback_head)) = presentation.head_stages().get(1)
        else {
            panic!("retained callback head keeps its source presentation");
        };
        assert_eq!(callback_head.source_param_count(), 1);
        let FacadeUse::Function { .. } = retained.plan().facade().use_at(callback_slots[0]) else {
            unreachable!()
        };
        let BoundaryFacadeExecutionUse::Function(callback_presentation) =
            presentation.root_uses().use_at(callback_slots[0])
        else {
            panic!("retained nested function keeps its source presentation");
        };
        assert_eq!(callback_presentation.source_param_count(), 1);
        assert!(matches!(
            prepared
                .shell_origins()
                .find(|(shell, _)| *shell == callback_shell)
                .map(|(_, origin)| origin),
            Some(BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: 2
            })
        ));
    }

    #[test]
    fn retained_collection_is_order_independent_and_deduplicates_shells() {
        let forward = replayed_interface(
            r#"signature fixture v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host type A;
        host type B;
        host fn first() -> (A | B);
        host fn second() -> (A | B);
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        first;
        second;
      }
    }
  }
}
"#,
        );
        let mut reverse = forward.clone();
        reverse.removed.reverse();
        let package = package_with_items("api", Vec::new());
        let forward = PreparedBoundaryCallableSites::collect(&package, Some(&forward))
            .expect("forward retained order");
        let reverse = PreparedBoundaryCallableSites::collect(&package, Some(&reverse))
            .expect("reverse retained order");

        assert_eq!(forward, reverse);
        assert_eq!(forward.sites().len(), 2);
        assert_eq!(forward.shells().len(), 1);
        assert_eq!(
            forward.shells().next().expect("shared sum").kind(),
            FacadeKind::Sum
        );
    }

    #[test]
    fn live_collection_couples_every_owner_to_its_authoritative_declaration() {
        let package = package_with_modules(vec![
            (
                "api",
                vec![
                    public_newtype("Boxed", nominal("payload", "Value")),
                    exported_function(
                        "send",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                            "payload",
                            product(nominal("payload", "Left"), nominal("payload", "Right")),
                        )])]),
                        bare("Boxed"),
                    ),
                    host_function(
                        "receive",
                        sum(nominal("payload", "Ok"), nominal("payload", "Error")),
                        nominal("payload", "Reply"),
                    ),
                ],
            ),
            (
                "payload",
                ["Value", "Left", "Right", "Ok", "Error", "Reply"]
                    .into_iter()
                    .map(roleless_host_type)
                    .collect(),
            ),
        ]);
        let send =
            prepare_exported_boundary_callable(&package, "api", "send").expect("exported function");
        let receive =
            prepare_host_boundary_callable(&package, "api", "receive").expect("host function");
        let constructor = prepare_newtype_constructor_boundary_callable(&package, "api", "Boxed")
            .expect("newtype constructor");
        let projector = prepare_newtype_projector_boundary_callable(&package, "api", "Boxed")
            .expect("newtype projector");
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");

        assert_eq!(send.site(), &export_site("api", "send"));
        PreparedBoundaryScheme::try_new(send.exact_scheme(), &BTreeSet::new())
            .expect("the export projection is qualified in its declaring module");
        let owners = prepared
            .sites()
            .map(|site| site.site().owner().clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            owners,
            BTreeSet::from([
                BoundaryFacadeSiteOwner::ExportedFunction {
                    name: "send".to_owned(),
                },
                BoundaryFacadeSiteOwner::HostFunction {
                    name: "receive".to_owned(),
                },
                BoundaryFacadeSiteOwner::NewtypeConstructor {
                    newtype: "Boxed".to_owned(),
                    member: "wrap".to_owned(),
                },
                BoundaryFacadeSiteOwner::NewtypeProjector {
                    newtype: "Boxed".to_owned(),
                    member: "unwrap".to_owned(),
                },
            ])
        );

        let send = prepared.site(send.site()).expect("stored export site");
        let entry = send.plan().entry();
        assert!(matches!(
            entry.head_stages[0],
            BoundaryCallableHeadStage::Value { slots } if slots.len() == 2
        ));
        assert!(matches!(
            send.plan().facade().use_at(entry.returned),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "Boxed")
        ));

        let receive = prepared.site(receive.site()).expect("stored host site");
        let receive_entry = receive.plan().entry();
        let BoundaryCallableHeadStage::Value { slots } = receive_entry.head_stages[0] else {
            panic!("host function must start with its value group");
        };
        assert!(matches!(
            receive.plan().facade().use_at(slots[0]),
            FacadeUse::Sum { .. }
        ));
        assert!(prepared.site(constructor.site()).is_some());
        assert!(prepared.site(projector.site()).is_some());
    }

    #[test]
    fn collect_live_enumerates_exact_bridged_public_surfaces() {
        let both = |name| public_newtype(name, nominal("host", "Payload"));
        let ctor_only = |name| {
            newtype_with_visibility(
                name,
                nominal("host", "Payload"),
                Visibility::Public,
                Visibility::Public,
                Visibility::Private,
            )
        };
        let projector_only = |name| {
            newtype_with_visibility(
                name,
                nominal("host", "Payload"),
                Visibility::Public,
                Visibility::Private,
                Visibility::Public,
            )
        };
        let opaque = |name| {
            newtype_with_visibility(
                name,
                nominal("host", "Payload"),
                Visibility::Public,
                Visibility::Private,
                Visibility::Private,
            )
        };
        let package = package_with_modules_and_bridge(
            vec![
                (
                    "api/one",
                    vec![
                        nullary_export("send", nominal("host", "Reply")),
                        function_with_visibility(
                            "hidden",
                            Signature::from_groups(Vec::new()),
                            nominal("host", "Reply"),
                            Visibility::Private,
                        ),
                        function_with_visibility(
                            "scoped",
                            Signature::from_groups(Vec::new()),
                            nominal("host", "Reply"),
                            scoped_visibility("api"),
                        ),
                        host_function(
                            "receive",
                            nominal("host", "Request"),
                            nominal("host", "Reply"),
                        ),
                        both("Both"),
                        ctor_only("CtorOnly"),
                        projector_only("ProjectorOnly"),
                        opaque("Opaque"),
                        newtype_with_visibility(
                            "PrivateOuter",
                            nominal("host", "Payload"),
                            Visibility::Private,
                            Visibility::Public,
                            Visibility::Public,
                        ),
                        newtype_with_visibility(
                            "ScopedOuter",
                            nominal("host", "Payload"),
                            scoped_visibility("api"),
                            Visibility::Public,
                            Visibility::Public,
                        ),
                    ],
                ),
                (
                    "api/two",
                    vec![
                        nullary_export("send", nominal("host", "Reply")),
                        host_function(
                            "receive",
                            nominal("host", "Request"),
                            nominal("host", "Reply"),
                        ),
                        both("Both"),
                    ],
                ),
                (
                    "unbridged",
                    vec![
                        nullary_export("send", nominal("host", "Reply")),
                        host_function(
                            "receive",
                            nominal("host", "Request"),
                            nominal("host", "Reply"),
                        ),
                        both("Both"),
                    ],
                ),
                (
                    "host",
                    ["Payload", "Reply", "Request"]
                        .into_iter()
                        .map(roleless_host_type)
                        .collect(),
                ),
            ],
            vec![vec![
                BridgeGlobSegment::Literal("api".to_owned()),
                BridgeGlobSegment::DoubleStar,
            ]],
        );
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");
        let expected = BTreeSet::from([
            export_site("api/one", "send"),
            host_site("api/one", "receive"),
            newtype_site("api/one", "Both", "wrap", NewtypeMemberRole::Constructor),
            newtype_site("api/one", "Both", "unwrap", NewtypeMemberRole::Projector),
            newtype_site(
                "api/one",
                "CtorOnly",
                "wrap",
                NewtypeMemberRole::Constructor,
            ),
            newtype_site(
                "api/one",
                "ProjectorOnly",
                "unwrap",
                NewtypeMemberRole::Projector,
            ),
            export_site("api/two", "send"),
            host_site("api/two", "receive"),
            newtype_site("api/two", "Both", "wrap", NewtypeMemberRole::Constructor),
            newtype_site("api/two", "Both", "unwrap", NewtypeMemberRole::Projector),
        ]);
        let actual = prepared
            .sites()
            .map(|site| {
                assert_stage_alignment(site.plan(), site.execution().expect("live site"));
                site.site().clone()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
        assert!(prepared.site(&export_site("api/one", "hidden")).is_none());
        assert!(prepared.site(&export_site("api/one", "scoped")).is_none());
        assert!(prepared.site(&export_site("unbridged", "send")).is_none());
        assert!(
            prepared
                .site(&newtype_site(
                    "api/one",
                    "Opaque",
                    "wrap",
                    NewtypeMemberRole::Constructor,
                ))
                .is_none()
        );
        assert_eq!(
            prepared
                .public_newtypes()
                .map(|entry| entry.name().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                semantic_name("api/one", "Both"),
                semantic_name("api/one", "CtorOnly"),
                semantic_name("api/one", "ProjectorOnly"),
                semantic_name("api/one", "Opaque"),
                semantic_name("api/two", "Both"),
            ])
        );
        assert!(
            !prepared
                .public_newtypes()
                .any(|entry| entry.name() == &semantic_name("unbridged", "Both"))
        );
    }

    #[test]
    fn execution_can_retain_a_unit_body_parameter_behind_a_nullary_facade() {
        let package = package_with_modules(vec![
            (
                "api",
                vec![
                    nullary_export("nullary", nominal("host", "Result")),
                    exported_function(
                        "unitful",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                            "value",
                            unit(),
                        )])]),
                        nominal("host", "Result"),
                    ),
                ],
            ),
            ("host", vec![roleless_host_type("Result")]),
        ]);
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");
        let nullary = prepared
            .site(&export_site("api", "nullary"))
            .expect("nullary site");
        let unitful = prepared
            .site(&export_site("api", "unitful"))
            .expect("unitful site");

        for site in [nullary, unitful] {
            assert!(matches!(
                site.plan().entry().head_stages[0],
                BoundaryCallableHeadStage::Value { slots } if slots.is_empty()
            ));
        }
        let [CallableExecutionStage::Value(nullary_layout)] =
            nullary.execution().expect("live site").head_stages()
        else {
            panic!("nullary export has one execution value stage");
        };
        assert_eq!(nullary_layout.source_param_count(), 0);
        assert_eq!(nullary_layout.body_abi_arity(), 0);
        assert_eq!(nullary_layout.facade_slot_count(), 0);
        assert!(nullary_layout.source_params().is_empty());

        let [CallableExecutionStage::Value(unitful_layout)] =
            unitful.execution().expect("live site").head_stages()
        else {
            panic!("unitful export has one execution value stage");
        };
        assert_eq!(unitful_layout.source_param_count(), 1);
        assert_eq!(unitful_layout.body_abi_arity(), 1);
        assert_eq!(unitful_layout.facade_slot_count(), 0);
        let [unitful_param] = unitful_layout.source_params() else {
            panic!("the private ABI retains one Unit source parameter");
        };
        assert_eq!(unitful_param.facade_slots(), 0..0);
        assert_eq!(
            unitful_param.adapter(),
            CallableSourceParamAdapter::UnitValue
        );
        assert!(unitful_param.product_shell().is_none());
        assert_eq!(
            validate_function_execution_layout(unitful_layout, 0),
            Ok(())
        );

        let nullary_root = expect_execution_function(
            nullary.execution().expect("live site").root_uses(),
            nullary.plan().facade().root(),
        );
        assert_eq!(nullary_root.source_param_count(), 0);
        assert!(nullary_root.source_params().is_empty());
        let unitful_root = expect_execution_function(
            unitful.execution().expect("live site").root_uses(),
            unitful.plan().facade().root(),
        );
        assert_eq!(unitful_root, nullary_root);
    }

    #[test]
    fn execution_ranges_preserve_source_params_and_expand_only_the_final_right_spine() {
        let package = package_with_modules(vec![
            (
                "api",
                vec![
                    exported_function(
                        "single_product",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                            "value",
                            product(nominal("host", "A"), nominal("host", "B")),
                        )])]),
                        nominal("host", "Reply"),
                    ),
                    exported_function(
                        "two_params",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![
                            value_param(
                                "first",
                                product(nominal("host", "A"), nominal("host", "B")),
                            ),
                            value_param(
                                "second",
                                product(nominal("host", "C"), nominal("host", "D")),
                            ),
                        ])]),
                        nominal("host", "Reply"),
                    ),
                    exported_function(
                        "applied_product",
                        Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
                            "value",
                            product(
                                nominal_args("host", "Box", vec![nominal("host", "A")]),
                                sum(
                                    nominal("host", "Token"),
                                    nominal_args("host", "Box", vec![nominal("host", "A")]),
                                ),
                            ),
                        )])]),
                        nominal("host", "Reply"),
                    ),
                ],
            ),
            ("host", {
                let mut items = ["A", "B", "C", "D", "Reply", "Token"]
                    .into_iter()
                    .map(roleless_host_type)
                    .collect::<Vec<_>>();
                items.push(host_type(
                    "Box",
                    vec![TypeParam {
                        name: "T".to_owned(),
                        span: span(),
                        kind: Some(Kind::Star),
                    }],
                    None,
                ));
                items
            }),
        ]);
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");

        let single_site = prepared
            .site(&export_site("api", "single_product"))
            .expect("single-product site");
        let single = execution_value_stage(single_site, 0);
        assert_eq!(single.source_param_count(), 1);
        assert_eq!(single.body_abi_arity(), 1);
        assert_eq!(single.facade_slot_count(), 2);
        let [param] = single.source_params() else {
            panic!("single source parameter");
        };
        assert_eq!(param.facade_slots(), 0..2);
        assert_eq!(param.adapter(), CallableSourceParamAdapter::RightNest);
        assert_eq!(
            param.product_shell(),
            Some(&FacadeShellId::new(
                FacadeKind::Product,
                vec![
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ],
            ))
        );
        assert_eq!(single.facade_shell(), param.product_shell());
        assert_eq!(
            expect_execution_function(
                single_site.execution().expect("live site").root_uses(),
                single_site.plan().facade().root(),
            ),
            single
        );

        let two_site = prepared
            .site(&export_site("api", "two_params"))
            .expect("two-parameter site");
        let two = execution_value_stage(two_site, 0);
        assert_eq!(two.source_param_count(), 2);
        assert_eq!(two.body_abi_arity(), 2);
        assert_eq!(two.facade_slot_count(), 3);
        let two_stage_shell = two.facade_shell().expect("three-slot facade shell");
        assert_eq!(two_stage_shell.kind(), FacadeKind::Product);
        assert_eq!(two_stage_shell.ordered_keys().len(), 3);
        assert!(prepared.shells().any(|shell| shell == two_stage_shell));
        let [first, second] = two.source_params() else {
            panic!("two source parameters");
        };
        assert_eq!(first.facade_slots(), 0..1);
        assert_eq!(first.adapter(), CallableSourceParamAdapter::Identity);
        assert!(first.product_shell().is_none());
        assert_eq!(second.facade_slots(), 1..3);
        assert_eq!(second.adapter(), CallableSourceParamAdapter::RightNest);
        assert_eq!(
            second.product_shell(),
            Some(&FacadeShellId::new(
                FacadeKind::Product,
                vec![
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ],
            ))
        );
        assert_eq!(
            expect_execution_function(
                two_site.execution().expect("live site").root_uses(),
                two_site.plan().facade().root(),
            ),
            two
        );

        let applied_site = prepared
            .site(&export_site("api", "applied_product"))
            .expect("applied-product site");
        let applied = execution_value_stage(applied_site, 0);
        let [param] = applied.source_params() else {
            panic!("one applied-product source parameter");
        };
        assert_eq!(param.adapter(), CallableSourceParamAdapter::RightNest);
        assert_eq!(
            param.product_shell(),
            Some(&FacadeShellId::new(
                FacadeKind::Product,
                vec![
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ],
            ))
        );
    }

    #[test]
    fn type_application_never_rescans_a_frozen_source_range() {
        let signature = Signature::from_groups(vec![
            SignatureGroup::Type(vec![TypeParam {
                name: "T".to_owned(),
                span: span(),
                kind: None,
            }]),
            SignatureGroup::Value(vec![value_param("value", bare("T"))]),
        ]);
        let package = package_with_modules(vec![
            (
                "api",
                vec![exported_function(
                    "generic",
                    signature,
                    nominal("host", "Reply"),
                )],
            ),
            ("host", vec![roleless_host_type("Reply")]),
        ]);
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");
        let generic = prepared
            .site(&export_site("api", "generic"))
            .expect("generic site");
        assert!(matches!(
            generic.execution().expect("live site").head_stages(),
            [
                CallableExecutionStage::Type {
                    action: CallableTypeStageAction::InvokeNullary,
                },
                CallableExecutionStage::Value(_),
            ]
        ));
        let value = execution_value_stage(generic, 1);
        let [param] = value.source_params() else {
            panic!("one frozen generic source parameter");
        };
        assert_eq!(param.facade_slots(), 0..1);
        assert_eq!(param.adapter(), CallableSourceParamAdapter::Identity);

        let product_argument = plan(&product(nominal("host", "Left"), nominal("host", "Right")));
        let applied = generic
            .plan()
            .facade()
            .apply_leading_type_arg(&product_argument)
            .expect("leading type stage");
        assert!(matches!(
            applied.use_at(applied.root()),
            FacadeUse::Function { slots, .. } if slots.len() == 1
        ));
        assert_eq!(param.facade_slots(), 0..1);
    }

    #[test]
    fn execution_layout_follows_interleaved_heads_and_excludes_returned_functions() {
        let interleaved = Signature::from_groups(vec![
            SignatureGroup::Value(vec![value_param("first", nominal("host", "A"))]),
            SignatureGroup::Type(vec![TypeParam {
                name: "T".to_owned(),
                span: span(),
                kind: None,
            }]),
            SignatureGroup::Value(vec![value_param("second", bare("T"))]),
        ]);
        let returned = Signature::from_groups(vec![SignatureGroup::Value(vec![value_param(
            "value",
            nominal("host", "A"),
        )])]);
        let package = package_with_modules(vec![
            (
                "api",
                vec![
                    exported_function("interleaved", interleaved, nominal("host", "Reply")),
                    exported_function(
                        "returned_function",
                        returned,
                        function(nominal("host", "B"), nominal("host", "Reply"), 1),
                    ),
                ],
            ),
            (
                "host",
                ["A", "B", "Reply"]
                    .into_iter()
                    .map(roleless_host_type)
                    .collect(),
            ),
        ]);
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");
        let interleaved = prepared
            .site(&export_site("api", "interleaved"))
            .expect("interleaved site");
        assert!(matches!(
            interleaved.execution().expect("live site").head_stages(),
            [
                CallableExecutionStage::Value(_),
                CallableExecutionStage::Type {
                    action: CallableTypeStageAction::InvokeNullary,
                },
                CallableExecutionStage::Value(_),
            ]
        ));
        assert_eq!(execution_value_stage(interleaved, 0).facade_slot_count(), 1);
        assert_eq!(execution_value_stage(interleaved, 2).facade_slot_count(), 1);
        let head_shell = interleaved
            .plan()
            .head_value_shell()
            .expect("two compacted head values own one exact shell");
        assert_eq!(head_shell.ordered_keys().len(), 2);
        assert!(prepared.shells().any(|shell| shell == head_shell));
        let interleaved_execution = interleaved.execution().expect("live site").root_uses();
        let facade = interleaved.plan().facade();
        let (_, type_head) = expect_function(facade, facade.root());
        let (_, final_value_head) = expect_forall(facade, type_head);
        let (_, returned_id) = expect_function(facade, final_value_head);
        assert_eq!(returned_id, interleaved.plan().entry().returned);
        assert!(matches!(
            interleaved_execution.use_at(facade.root()),
            BoundaryFacadeExecutionUse::Function(_)
        ));
        assert!(matches!(
            interleaved_execution.use_at(type_head),
            BoundaryFacadeExecutionUse::InvokeForall
        ));
        assert!(matches!(
            interleaved_execution.use_at(final_value_head),
            BoundaryFacadeExecutionUse::Function(_)
        ));

        let returned = prepared
            .site(&export_site("api", "returned_function"))
            .expect("returned-function site");
        assert_eq!(
            returned.execution().expect("live site").head_stages().len(),
            1
        );
        let entry = returned.plan().entry();
        assert!(matches!(
            returned.plan().facade().use_at(entry.returned),
            FacadeUse::Function { .. }
        ));
        assert!(matches!(
            returned
                .execution()
                .expect("live site")
                .root_uses()
                .use_at(entry.returned),
            BoundaryFacadeExecutionUse::Function(_)
        ));
    }

    #[test]
    fn authoritative_projection_deeply_unfolds_aliases_without_losing_the_raw_cut() {
        let alias_param = TypeParam {
            name: "A".to_owned(),
            span: span(),
            kind: None,
        };
        let dependency_items = vec![
            public_alias(
                "Pair",
                vec![alias_param.clone()],
                product(bare("A"), nominal("host", "Tail")),
            ),
            public_alias(
                "Choice",
                Vec::new(),
                sum(nominal("host", "Left"), nominal("host", "Right")),
            ),
            public_alias(
                "Callback",
                Vec::new(),
                function(nominal("host", "Input"), nominal("host", "Output"), 1),
            ),
            public_alias(
                "Bundle",
                vec![alias_param],
                product(bare_args("Pair", vec![bare("A")]), bare("Choice")),
            ),
        ];
        let signature = Signature::from_groups(vec![
            SignatureGroup::Type(vec![TypeParam {
                name: "T".to_owned(),
                span: span(),
                kind: None,
            }]),
            SignatureGroup::Value(vec![value_param(
                "payload",
                nominal_args("dep", "Bundle", vec![bare("T")]),
            )]),
        ]);
        let api_items = vec![
            public_alias("T", Vec::new(), nominal("host", "Wrong")),
            exported_function("consume", signature, nominal("dep", "Callback")),
        ];
        let package = package_with_modules(vec![
            ("api", api_items),
            ("dep", dependency_items),
            (
                "host",
                ["Tail", "Left", "Right", "Input", "Output"]
                    .into_iter()
                    .map(roleless_host_type)
                    .collect(),
            ),
        ]);
        let projection = prepare_exported_boundary_callable(&package, "api", "consume")
            .expect("cross-module aliases");
        assert_eq!(
            projection.head_stage_kinds(),
            &[
                BoundaryCallableHeadStageKind::Type,
                BoundaryCallableHeadStageKind::Value,
            ]
        );

        let Type::Forall { body, .. } = projection.exact_scheme() else {
            panic!("exact declaration scheme must retain its type stage");
        };
        let Type::Function { param, ret, .. } = body.as_ref() else {
            panic!("exact declaration scheme must retain its value stage");
        };
        assert!(matches!(
            param.as_ref(),
            Type::Path { segments, .. }
                if segments.last().is_some_and(|segment| segment.as_str() == "Bundle")
        ));
        assert!(matches!(
            ret.as_ref(),
            Type::Path { segments, .. }
                if segments.last().is_some_and(|segment| segment.as_str() == "Callback")
        ));

        let Type::Forall { body, .. } = projection.semantic_scheme() else {
            panic!("semantic scheme must retain the declaration type stage");
        };
        let Type::Function { param, ret, .. } = body.as_ref() else {
            panic!("semantic scheme must retain the declaration value stage");
        };
        assert!(matches!(param.as_ref(), Type::Product { .. }));
        assert!(matches!(ret.as_ref(), Type::Function { .. }));

        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");
        let callable = prepared
            .site(projection.site())
            .expect("stored alias-canonical plan");
        assert_eq!(
            callable.plan().head_stage_kinds(),
            &[
                BoundaryCallableHeadStageKind::Type,
                BoundaryCallableHeadStageKind::Value,
            ]
        );
        let entry = callable.plan().entry();
        let BoundaryCallableHeadStage::Type { id: binder, .. } = entry.head_stages[0] else {
            panic!("first declaration stage must be the T binder");
        };
        let BoundaryCallableHeadStage::Value { slots } = entry.head_stages[1] else {
            panic!("second declaration stage must be the payload");
        };
        assert_eq!(slots.len(), 2);
        let FacadeUse::Product { args, .. } = callable.plan().facade().use_at(slots[0]) else {
            panic!("nested Pair alias must become a product use");
        };
        assert!(matches!(
            callable.plan().facade().use_at(args[0]),
            FacadeUse::Bound { binder: actual, .. } if actual == &binder
        ));
        assert!(matches!(
            callable.plan().facade().use_at(slots[1]),
            FacadeUse::Sum { .. }
        ));
        assert!(matches!(
            callable.plan().facade().use_at(entry.returned),
            FacadeUse::Function { .. }
        ));
    }

    #[test]
    fn execution_layout_deeply_canonicalizes_aliases_but_stops_at_nominals() {
        let parameter = |name, ty| {
            exported_function(
                name,
                Signature::from_groups(vec![SignatureGroup::Value(vec![value_param("value", ty)])]),
                nominal("host", "Reply"),
            )
        };
        let package = package_with_modules(vec![
            (
                "api",
                vec![
                    public_alias("Nothing", Vec::new(), unit()),
                    public_alias(
                        "Pair",
                        Vec::new(),
                        product(nominal("host", "A"), nominal("host", "B")),
                    ),
                    public_alias(
                        "Choice",
                        Vec::new(),
                        sum(nominal("host", "A"), nominal("host", "B")),
                    ),
                    public_alias(
                        "Callback",
                        Vec::new(),
                        function(nominal("host", "A"), nominal("host", "B"), 1),
                    ),
                    public_newtype("Node", product(nominal("host", "Value"), bare("Node"))),
                    parameter("unit_alias", bare("Nothing")),
                    parameter("product_alias", bare("Pair")),
                    parameter("sum_alias", bare("Choice")),
                    parameter("function_alias", bare("Callback")),
                    parameter("recursive_nominal", bare("Node")),
                ],
            ),
            (
                "host",
                ["A", "B", "Value", "Reply"]
                    .into_iter()
                    .map(roleless_host_type)
                    .collect(),
            ),
        ]);
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");

        let unit_alias = prepared
            .site(&export_site("api", "unit_alias"))
            .expect("Unit alias site");
        let [unit_param] = execution_value_stage(unit_alias, 0).source_params() else {
            panic!("one Unit-alias source parameter");
        };
        assert_eq!(unit_param.facade_slots(), 0..1);
        assert_eq!(unit_param.adapter(), CallableSourceParamAdapter::Identity);

        let product_alias = prepared
            .site(&export_site("api", "product_alias"))
            .expect("product alias site");
        let [product_param] = execution_value_stage(product_alias, 0).source_params() else {
            panic!("one product-alias source parameter");
        };
        assert_eq!(product_param.facade_slots(), 0..2);
        assert_eq!(
            product_param.adapter(),
            CallableSourceParamAdapter::RightNest
        );

        for name in ["sum_alias", "function_alias", "recursive_nominal"] {
            let site = prepared
                .site(&export_site("api", name))
                .expect("identity-adapted site");
            let [param] = execution_value_stage(site, 0).source_params() else {
                panic!("one identity-adapted source parameter");
            };
            assert_eq!(param.facade_slots(), 0..1);
            assert_eq!(param.adapter(), CallableSourceParamAdapter::Identity);
        }
        let nominal = prepared
            .site(&export_site("api", "recursive_nominal"))
            .expect("recursive nominal site");
        let BoundaryCallableHeadStage::Value { slots } = nominal.plan().entry().head_stages[0]
        else {
            panic!("nominal parameter value stage");
        };
        assert!(matches!(
            nominal.plan().facade().use_at(slots[0]),
            FacadeUse::Nominal { name, .. } if name == &semantic_name("api", "Node")
        ));
    }

    #[test]
    fn catalog_enumerates_every_nested_shell_from_the_stored_callable_plan() {
        let nested = product(
            product(nominal("record", "First"), nominal("record", "Second")),
            sum(
                nominal("choice", "Left"),
                sum(nominal("choice", "Middle"), nominal("choice", "Right")),
            ),
        );
        let opaque = |name| {
            newtype_with_visibility(
                name,
                unit(),
                Visibility::Public,
                Visibility::Private,
                Visibility::Private,
            )
        };
        let package = package_with_modules(vec![
            ("api", vec![nullary_export("generic", nested)]),
            ("record", vec![opaque("First"), opaque("Second")]),
            (
                "choice",
                vec![opaque("Left"), opaque("Middle"), opaque("Right")],
            ),
        ]);
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");
        let callable = prepared
            .site(&export_site("api", "generic"))
            .expect("stored nested plan");
        assert!(matches!(
            callable.plan().entry().head_stages[0],
            BoundaryCallableHeadStage::Value { slots } if slots.is_empty()
        ));
        let expected = BTreeSet::from([
            FacadeShellId::new(
                FacadeKind::Product,
                vec![
                    SemanticKey::Positional { index: 0 },
                    SemanticKey::Positional { index: 1 },
                ],
            ),
            FacadeShellId::new(
                FacadeKind::Product,
                vec![
                    SemanticKey::Bare {
                        name: "First".to_owned(),
                    },
                    SemanticKey::Bare {
                        name: "Second".to_owned(),
                    },
                ],
            ),
            FacadeShellId::new(
                FacadeKind::Sum,
                vec![
                    SemanticKey::Bare {
                        name: "Left".to_owned(),
                    },
                    SemanticKey::Bare {
                        name: "Middle".to_owned(),
                    },
                    SemanticKey::Bare {
                        name: "Right".to_owned(),
                    },
                ],
            ),
        ]);
        assert_eq!(
            prepared.shells().cloned().collect::<BTreeSet<_>>(),
            expected
        );
    }

    #[test]
    fn existential_projector_keeps_returned_cps_stages_outside_the_head_cut() {
        let Item::Newtype(mut packed) = public_newtype("Packed", bare("Hidden")) else {
            unreachable!("test helper returned a newtype");
        };
        packed.existential_params.push(TypeParam {
            name: "Hidden".to_owned(),
            span: span(),
            kind: None,
        });
        let package = package_with_items("api", vec![Item::Newtype(packed)]);
        let constructor = prepare_newtype_constructor_boundary_callable(&package, "api", "Packed")
            .expect("existential constructor");
        let projector = prepare_newtype_projector_boundary_callable(&package, "api", "Packed")
            .expect("existential projector");
        assert!(matches!(
            constructor.source_head_stages(),
            [
                AuthoritativeBoundaryHeadStage::Type { .. },
                AuthoritativeBoundaryHeadStage::Value { source_params }
            ] if source_params.len() == 1
        ));
        assert!(matches!(
            projector.source_head_stages(),
            [AuthoritativeBoundaryHeadStage::Value { source_params }]
                if source_params.len() == 1
        ));
        assert!(matches!(
            projector.exact_scheme(),
            Type::Function { ret, .. } if matches!(ret.as_ref(), Type::Forall { .. })
        ));
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&package).expect("complete live sites");

        let constructor = prepared.site(constructor.site()).expect("constructor plan");
        assert_eq!(
            constructor.plan().head_stage_kinds(),
            &[
                BoundaryCallableHeadStageKind::Type,
                BoundaryCallableHeadStageKind::Value,
            ]
        );
        let projector = prepared.site(projector.site()).expect("projector plan");
        assert_eq!(
            projector.plan().head_stage_kinds(),
            &[BoundaryCallableHeadStageKind::Value]
        );
        assert!(matches!(
            projector
                .plan()
                .facade()
                .use_at(projector.plan().entry().returned),
            FacadeUse::Forall { .. }
        ));
        assert!(matches!(
            projector.execution().expect("live site").head_stages(),
            [CallableExecutionStage::Value(_)]
        ));
        let compactable = projector
            .execution()
            .expect("live site")
            .direct_projector_compactable_foralls();
        assert_eq!(compactable.len(), 2);
        assert!(compactable.contains(&projector.plan().entry().returned));
        assert!(compactable.iter().all(|use_id| matches!(
            projector.plan().facade().use_at(*use_id),
            FacadeUse::Forall { .. }
        )));
    }

    fn right_nested(
        mut slots: Vec<Type<Routed>>,
        join: fn(Type<Routed>, Type<Routed>) -> Type<Routed>,
    ) -> Type<Routed> {
        let mut current = slots.pop().expect("at least one slot");
        while let Some(left) = slots.pop() {
            current = join(left, current);
        }
        current
    }

    #[test]
    fn width_1024_product_and_sum_planning_is_stack_safe() {
        let product_ty = right_nested(
            (0..1024).map(|_| nominal("host", "Scalar")).collect(),
            product,
        );
        let sum_ty = right_nested((0..1024).map(|_| nominal("host", "Scalar")).collect(), sum);
        let product_plan = plan(&product_ty);
        let sum_plan = plan(&sum_ty);

        assert!(matches!(
            product_plan.use_at(product_plan.root()),
            FacadeUse::Product { args, shell, .. }
                if args.len() == 1024 && shell.ordered_keys().len() == 1024
        ));
        assert!(matches!(
            sum_plan.use_at(sum_plan.root()),
            FacadeUse::Sum { args, shell, .. }
                if args.len() == 1024 && shell.ordered_keys().len() == 1024
        ));
    }

    #[test]
    fn routed_function_capabilities_survive_planning_and_substitution() {
        let raw = forall(
            "T",
            Type::Function {
                param: Box::new(bare("T")),
                ret: Box::new(nominal("api", "R")),
                meta: meta(),
                abi_arity: 123,
                caps: FnTypeCapabilities {
                    lifetime: Lifetime::Stack,
                },
            },
        );
        let applied = plan(&raw)
            .apply_leading_type_arg(&plan(&nominal("host", "Item")))
            .expect("type stage");
        let FacadeUse::Function { caps, slots, .. } = applied.use_at(applied.root()) else {
            panic!("expected function");
        };
        assert_eq!(caps.lifetime, Lifetime::Stack);
        assert_eq!(slots.len(), 1);
    }
}
