//! Pure public-name codecs and namespace claims for the Go backend.
//!
//! Every choice in this module depends only on semantic identity and fixed Go
//! syntax. In particular, neither source declaration order nor namespace
//! occupancy participates in choosing a spelling. Later composition can
//! therefore reject a collision without renaming an existing public symbol.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;

use crate::backends::boundary_facade::{FacadeShellId, SemanticKey};

const SEMANTIC_KEY_CODEC_PREFIX: &str = "KioKey_V";
const SEMANTIC_KEY_CODEC_VERSION: u64 = 1;

/// The public role in which a semantic product or sum key is rendered.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum GoSemanticKeyRole {
    ProductField,
    SumArm,
}

impl GoSemanticKeyRole {
    fn positional_prefix(self) -> u8 {
        match self {
            Self::ProductField => b'F',
            Self::SumArm => b'K',
        }
    }
}

/// Failure to decode a canonical public name or construct a Go identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GoNamingError {
    reason: &'static str,
}

impl GoNamingError {
    const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl fmt::Display for GoNamingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for GoNamingError {}

/// One already-validated non-blank ASCII Go namespace identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GoIdentifier(String);

impl GoIdentifier {
    pub(crate) fn new(spelling: String) -> Result<Self, GoNamingError> {
        if is_ascii_go_identifier(&spelling) {
            Ok(Self(spelling))
        } else {
            Err(GoNamingError::new("invalid ASCII Go identifier"))
        }
    }

    fn new_exported(spelling: String) -> Result<Self, GoNamingError> {
        if is_exported_ascii_go_identifier(&spelling) {
            Ok(Self(spelling))
        } else {
            Err(GoNamingError::new("invalid exported ASCII Go identifier"))
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GoIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Encode a semantic key as its canonical exported Go identifier.
///
/// A bare key keeps its word-cased spelling only when that spelling is
/// already unambiguous in this role. Qualified keys always carry explicit
/// component frames. Positional keys use the role-reserved `F<n>` or `K<n>`
/// class with canonical decimal indices.
pub(crate) fn encode_semantic_key(role: GoSemanticKeyRole, key: &SemanticKey) -> String {
    match key {
        SemanticKey::Bare { name }
            if is_natural_bare_spelling(
                role,
                &crate::backends::public_names::host_name_core(name),
            ) =>
        {
            crate::backends::public_names::host_name_core(name)
        }
        SemanticKey::Bare { name } => {
            let mut encoded = framed_prefix();
            write_component_frame(&mut encoded, 'B', name);
            encoded
        }
        SemanticKey::Qualified {
            module_segments,
            name,
        } => {
            let mut encoded = framed_prefix();
            write!(&mut encoded, "Q{}_", module_segments.len())
                .expect("writing to String cannot fail");
            for segment in module_segments {
                write_component_frame(&mut encoded, 'M', segment);
            }
            write_component_frame(&mut encoded, 'N', name);
            encoded
        }
        SemanticKey::Positional { index } => {
            format!("{}{}", char::from(role.positional_prefix()), index)
        }
    }
}

/// Decode only canonical semantic-key spellings for the given public role.
#[cfg(test)]
pub(crate) fn decode_semantic_key(
    role: GoSemanticKeyRole,
    spelling: &str,
) -> Result<SemanticKey, GoNamingError> {
    let decoded = if spelling.starts_with(SEMANTIC_KEY_CODEC_PREFIX) {
        decode_framed_semantic_key(spelling)?
    } else if let Some(digits) = positional_digits(role, spelling) {
        let index = parse_canonical_decimal(digits)?;
        SemanticKey::Positional { index }
    } else if is_natural_bare_spelling(role, spelling) {
        SemanticKey::Bare {
            name: source_component(spelling),
        }
    } else {
        return Err(GoNamingError::new(
            "spelling is not a canonical Go semantic key",
        ));
    };

    if encode_semantic_key(role, &decoded) != spelling {
        return Err(GoNamingError::new("noncanonical Go semantic-key spelling"));
    }
    Ok(decoded)
}

/// Render the canonical facade-shell identity as an exported Go name.
///
/// Go has one facade-shell ABI: [`FacadeShellId::encode_public`]. The exact
/// fallback codec is not an alternate public spelling for this backend.
/// Namespace occupancy is deliberately absent from this API. A collision is a
/// composition error, not a reason to change this identity-derived choice.
pub(crate) fn facade_shell_go_identifier(shell: &FacadeShellId) -> GoIdentifier {
    GoIdentifier::new_exported(shell.encode_public()).unwrap_or_else(|_| {
        unreachable!("FacadeShellId::encode_public must produce an exported ASCII Go identifier")
    })
}

/// Immutable claims grouped by namespace scope.
///
/// `S`, `N`, and `O` deliberately remain generic: a composed naming plan can
/// define its scopes and owner identities without teaching this validator any
/// emitter-specific roles. Repeating the same `(scope, name, owner)` claim is
/// idempotent; assigning the same name in one scope to another owner is an
/// error. The complete input is canonicalized before validation, so the first
/// reported `(scope, name)` and its first two owners are independent of input
/// iteration order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopeClaims<S, N, O> {
    by_scope: BTreeMap<S, BTreeMap<N, O>>,
}

impl<S, N, O> ScopeClaims<S, N, O>
where
    S: Clone + Ord,
    N: Clone + Ord,
    O: Clone + Ord,
{
    pub(crate) fn try_collect<I>(claims: I) -> Result<Self, ScopeClaimCollision<S, N, O>>
    where
        I: IntoIterator<Item = (S, N, O)>,
    {
        let mut grouped: BTreeMap<S, BTreeMap<N, BTreeSet<O>>> = BTreeMap::new();
        for (scope, name, owner) in claims {
            grouped
                .entry(scope)
                .or_default()
                .entry(name)
                .or_default()
                .insert(owner);
        }

        for (scope, scope_claims) in &grouped {
            for (name, owners) in scope_claims {
                if owners.len() > 1 {
                    let mut owners = owners.iter();
                    let first_owner = owners
                        .next()
                        .expect("a conflicting owner set has a first owner")
                        .clone();
                    let second_owner = owners
                        .next()
                        .expect("a conflicting owner set has a second owner")
                        .clone();
                    return Err(ScopeClaimCollision {
                        scope: scope.clone(),
                        name: name.clone(),
                        first_owner,
                        second_owner,
                    });
                }
            }
        }

        let by_scope = grouped
            .into_iter()
            .map(|(scope, scope_claims)| {
                let scope_claims = scope_claims
                    .into_iter()
                    .map(|(name, owners)| {
                        let owner = owners
                            .into_iter()
                            .next()
                            .expect("every grouped claim has one owner");
                        (name, owner)
                    })
                    .collect();
                (scope, scope_claims)
            })
            .collect();
        Ok(Self { by_scope })
    }

    #[cfg(test)]
    pub(crate) fn owner(&self, scope: &S, name: &N) -> Option<&O> {
        self.by_scope
            .get(scope)
            .and_then(|scope_claims| scope_claims.get(name))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_scope.values().map(BTreeMap::len).sum()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.by_scope.values().all(BTreeMap::is_empty)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&S, &N, &O)> {
        self.by_scope.iter().flat_map(|(scope, scope_claims)| {
            scope_claims
                .iter()
                .map(move |(name, owner)| (scope, name, owner))
        })
    }
}

/// The canonical first non-idempotent claim within one namespace scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopeClaimCollision<S, N, O> {
    scope: S,
    name: N,
    first_owner: O,
    second_owner: O,
}

impl<S, N, O> ScopeClaimCollision<S, N, O> {
    pub(crate) fn scope(&self) -> &S {
        &self.scope
    }

    pub(crate) fn name(&self) -> &N {
        &self.name
    }

    pub(crate) fn first_owner(&self) -> &O {
        &self.first_owner
    }

    pub(crate) fn second_owner(&self) -> &O {
        &self.second_owner
    }
}

fn framed_prefix() -> String {
    format!("{SEMANTIC_KEY_CODEC_PREFIX}{SEMANTIC_KEY_CODEC_VERSION}_")
}

fn is_natural_bare_spelling(role: GoSemanticKeyRole, spelling: &str) -> bool {
    is_exported_ascii_go_identifier(spelling)
        && !spelling.starts_with(SEMANTIC_KEY_CODEC_PREFIX)
        && positional_digits(role, spelling).is_none()
}

fn positional_digits(role: GoSemanticKeyRole, spelling: &str) -> Option<&str> {
    let bytes = spelling.as_bytes();
    let (prefix, digits) = bytes.split_first()?;
    if *prefix != role.positional_prefix()
        || digits.is_empty()
        || !digits.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some(&spelling[1..])
}

#[cfg(test)]
fn parse_canonical_decimal(digits: &str) -> Result<u64, GoNamingError> {
    if digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return Err(GoNamingError::new("noncanonical decimal frame"));
    }
    digits
        .parse()
        .map_err(|_| GoNamingError::new("decimal frame exceeds u64"))
}

#[cfg(test)]
fn decode_framed_semantic_key(spelling: &str) -> Result<SemanticKey, GoNamingError> {
    let mut cursor = TextCursor::new(spelling);
    cursor.consume_bytes(SEMANTIC_KEY_CODEC_PREFIX.as_bytes())?;
    let version = cursor.read_decimal_frame()?;
    if version != SEMANTIC_KEY_CODEC_VERSION {
        return Err(GoNamingError::new(
            "unsupported Go semantic-key codec version",
        ));
    }

    match cursor.peek() {
        Some(b'B') => {
            let name = source_component(&cursor.read_component(b'B')?);
            cursor.require_end()?;
            Ok(SemanticKey::Bare { name })
        }
        Some(b'Q') => {
            cursor.consume(b'Q')?;
            let segment_count = cursor.read_decimal_frame()?;
            let mut module_segments = Vec::new();
            for _ in 0..segment_count {
                module_segments.push(source_component(&cursor.read_component(b'M')?));
            }
            let name = source_component(&cursor.read_component(b'N')?);
            cursor.require_end()?;
            Ok(SemanticKey::Qualified {
                module_segments,
                name,
            })
        }
        _ => Err(GoNamingError::new("unknown Go semantic-key frame role")),
    }
}

fn write_component_frame(target: &mut String, tag: char, component: &str) {
    let escaped = escape_component(&crate::backends::public_names::host_name_core(component));
    write!(target, "{tag}{}_{}", escaped.len(), escaped).expect("writing to String cannot fail");
}

#[cfg(test)]
fn source_component(rendered: &str) -> String {
    let mut source = String::new();
    let mut first = true;
    for ch in rendered.chars() {
        if ch.is_ascii_uppercase() && !first {
            source.push('_');
            source.push(ch.to_ascii_lowercase());
        } else {
            source.push(ch);
        }
        if ch != '_' {
            first = false;
        }
    }
    source
}

fn escape_component(component: &str) -> String {
    let mut escaped = String::new();
    for byte in component.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => escaped.push(char::from(byte)),
            b'_' => escaped.push_str("_u"),
            _ => write!(&mut escaped, "_x{byte:02x}").expect("writing to String cannot fail"),
        }
    }
    escaped
}

#[cfg(test)]
fn unescape_component(escaped: &[u8]) -> Result<String, GoNamingError> {
    let mut bytes = Vec::new();
    let mut offset = 0;
    while offset < escaped.len() {
        match escaped[offset] {
            byte @ (b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9') => {
                bytes.push(byte);
                offset += 1;
            }
            b'_' if escaped.get(offset + 1) == Some(&b'u') => {
                bytes.push(b'_');
                offset += 2;
            }
            b'_' if escaped.get(offset + 1) == Some(&b'x') => {
                let high = *escaped
                    .get(offset + 2)
                    .ok_or_else(|| GoNamingError::new("truncated hexadecimal escape"))?;
                let low = *escaped
                    .get(offset + 3)
                    .ok_or_else(|| GoNamingError::new("truncated hexadecimal escape"))?;
                bytes.push(hex_value(high)? * 16 + hex_value(low)?);
                offset += 4;
            }
            b'_' => return Err(GoNamingError::new("unknown component escape")),
            _ => return Err(GoNamingError::new("non-ASCII byte in component frame")),
        }
    }
    String::from_utf8(bytes).map_err(|_| GoNamingError::new("component frame is not valid UTF-8"))
}

#[cfg(test)]
fn hex_value(byte: u8) -> Result<u8, GoNamingError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(GoNamingError::new("invalid lowercase hexadecimal digit")),
    }
}

fn is_ascii_go_identifier(spelling: &str) -> bool {
    if spelling == "_" {
        return false;
    }
    let Some((first, rest)) = spelling.as_bytes().split_first() else {
        return false;
    };
    matches!(*first, b'A'..=b'Z' | b'a'..=b'z' | b'_')
        && rest
            .iter()
            .all(|byte| matches!(*byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
        && !is_go_keyword(spelling)
}

fn is_exported_ascii_go_identifier(spelling: &str) -> bool {
    spelling
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_uppercase)
        && is_ascii_go_identifier(spelling)
}

fn is_go_keyword(spelling: &str) -> bool {
    matches!(
        spelling,
        "break"
            | "default"
            | "func"
            | "interface"
            | "select"
            | "case"
            | "defer"
            | "go"
            | "map"
            | "struct"
            | "chan"
            | "else"
            | "goto"
            | "package"
            | "switch"
            | "const"
            | "fallthrough"
            | "if"
            | "range"
            | "type"
            | "continue"
            | "for"
            | "import"
            | "return"
            | "var"
    )
}

#[cfg(test)]
struct TextCursor<'a> {
    input: &'a [u8],
    offset: usize,
}

#[cfg(test)]
impl<'a> TextCursor<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            offset: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.offset).copied()
    }

    fn consume(&mut self, expected: u8) -> Result<(), GoNamingError> {
        if self.peek() != Some(expected) {
            return Err(GoNamingError::new(
                "unexpected byte in Go semantic-key frame",
            ));
        }
        self.offset += 1;
        Ok(())
    }

    fn consume_bytes(&mut self, expected: &[u8]) -> Result<(), GoNamingError> {
        let end = self
            .offset
            .checked_add(expected.len())
            .ok_or_else(|| GoNamingError::new("semantic-key frame length overflow"))?;
        if self.input.get(self.offset..end) != Some(expected) {
            return Err(GoNamingError::new("invalid Go semantic-key frame prefix"));
        }
        self.offset = end;
        Ok(())
    }

    fn read_decimal_frame(&mut self) -> Result<u64, GoNamingError> {
        let start = self.offset;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.offset += 1;
        }
        if self.offset == start {
            return Err(GoNamingError::new("missing decimal frame"));
        }
        let digits = std::str::from_utf8(&self.input[start..self.offset])
            .expect("an ASCII digit sequence is valid UTF-8");
        let value = parse_canonical_decimal(digits)?;
        self.consume(b'_')?;
        Ok(value)
    }

    fn read_component(&mut self, tag: u8) -> Result<String, GoNamingError> {
        self.consume(tag)?;
        let escaped_len = usize::try_from(self.read_decimal_frame()?)
            .map_err(|_| GoNamingError::new("component frame length exceeds usize"))?;
        let end = self
            .offset
            .checked_add(escaped_len)
            .ok_or_else(|| GoNamingError::new("component frame length overflow"))?;
        let escaped = self
            .input
            .get(self.offset..end)
            .ok_or_else(|| GoNamingError::new("truncated component frame"))?;
        self.offset = end;
        unescape_component(escaped)
    }

    fn require_end(&self) -> Result<(), GoNamingError> {
        if self.offset == self.input.len() {
            Ok(())
        } else {
            Err(GoNamingError::new(
                "trailing data after Go semantic-key frame",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::boundary_facade::FacadeKind;

    fn bare(name: &str) -> SemanticKey {
        SemanticKey::Bare {
            name: name.to_owned(),
        }
    }

    fn qualified(module_segments: &[&str], name: &str) -> SemanticKey {
        SemanticKey::Qualified {
            module_segments: module_segments
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect(),
            name: name.to_owned(),
        }
    }

    fn identifier(spelling: &str) -> GoIdentifier {
        GoIdentifier::new(spelling.to_owned()).expect("test identifier must be legal")
    }

    #[test]
    fn go_blank_identifier_is_not_a_namespace_claim() {
        assert!(GoIdentifier::new("_".to_owned()).is_err());
        assert!(GoIdentifier::new("_value".to_owned()).is_ok());
    }

    #[test]
    fn natural_bare_spelling_is_exact_and_round_trips() {
        let key = bare("Foo_bar");
        let encoded = encode_semantic_key(GoSemanticKeyRole::ProductField, &key);

        assert_eq!(encoded, "FooBar");
        assert_eq!(
            decode_semantic_key(GoSemanticKeyRole::ProductField, &encoded),
            Ok(key)
        );
    }

    #[test]
    fn underscores_survive_natural_and_framed_spellings() {
        let natural = bare("Foo_bar__");
        assert_eq!(
            encode_semantic_key(GoSemanticKeyRole::SumArm, &natural),
            "FooBar__"
        );

        let framed = bare("_Foo_bar");
        let encoded = encode_semantic_key(GoSemanticKeyRole::SumArm, &framed);
        assert!(encoded.starts_with(SEMANTIC_KEY_CODEC_PREFIX));
        assert!(encoded.contains("_u"));
        assert_eq!(
            decode_semantic_key(GoSemanticKeyRole::SumArm, &encoded),
            Ok(framed)
        );
    }

    #[test]
    fn framed_prefix_is_reserved_to_avoid_a_natural_bare_collision() {
        let key = bare("Kio_key_v1_b0_");
        let encoded = encode_semantic_key(GoSemanticKeyRole::ProductField, &key);
        assert_eq!(encoded, "KioKeyV1B0_");
        assert!(!encoded.starts_with(SEMANTIC_KEY_CODEC_PREFIX));
        assert_eq!(
            decode_semantic_key(GoSemanticKeyRole::ProductField, &encoded),
            Ok(key)
        );
    }

    #[test]
    fn positional_looking_bare_keys_are_framed_for_their_role() {
        for (role, prefix) in [
            (GoSemanticKeyRole::ProductField, "F"),
            (GoSemanticKeyRole::SumArm, "K"),
        ] {
            for suffix in ["0", "01"] {
                let key = bare(&format!("{prefix}{suffix}"));
                let encoded = encode_semantic_key(role, &key);
                assert!(encoded.starts_with(SEMANTIC_KEY_CODEC_PREFIX));
                assert_eq!(decode_semantic_key(role, &encoded), Ok(key));
            }

            let positional = SemanticKey::Positional { index: 0 };
            assert_eq!(encode_semantic_key(role, &positional), format!("{prefix}0"));
            assert_eq!(
                decode_semantic_key(role, &format!("{prefix}0")),
                Ok(positional)
            );
            assert!(decode_semantic_key(role, &format!("{prefix}01")).is_err());
        }

        assert_eq!(
            encode_semantic_key(GoSemanticKeyRole::SumArm, &bare("F0")),
            "F0"
        );
        assert_eq!(
            encode_semantic_key(GoSemanticKeyRole::ProductField, &bare("K0")),
            "K0"
        );
    }

    #[test]
    fn qualified_frames_preserve_module_boundaries() {
        let left = qualified(&["a_b", "c"], "Thing");
        let right = qualified(&["a", "b_c"], "Thing");
        let left_encoded = encode_semantic_key(GoSemanticKeyRole::ProductField, &left);
        let right_encoded = encode_semantic_key(GoSemanticKeyRole::ProductField, &right);

        assert_ne!(left_encoded, right_encoded);
        assert_eq!(
            decode_semantic_key(GoSemanticKeyRole::ProductField, &left_encoded),
            Ok(left)
        );
        assert_eq!(
            decode_semantic_key(GoSemanticKeyRole::ProductField, &right_encoded),
            Ok(right)
        );
    }

    #[test]
    fn unicode_round_trips_and_invalid_utf8_is_rejected() {
        for source in ["Å_😀", "δ", "日本", "値"] {
            assert_eq!(
                unescape_component(escape_component(source).as_bytes()),
                Ok(source.to_owned())
            );
        }

        assert!(decode_semantic_key(GoSemanticKeyRole::ProductField, "KioKey_V1_B4__xff").is_err());
    }

    #[test]
    fn malformed_and_noncanonical_frames_are_rejected() {
        let malformed = [
            "KioKey_V_B0_",
            "KioKey_V01_B0_",
            "KioKey_V2_B0_",
            "KioKey_V1_X0_",
            "KioKey_V1_B1_",
            "KioKey_V1_B2__q",
            "KioKey_V1_B4__xFF",
            "KioKey_V1_B4__x5f",
            "KioKey_V1_B3_Foo",
            "KioKey_V1_B0_x",
            "KioKey_V1_Q1_N1_A",
            "KioKey_V1_Q0_N1_Ax",
        ];

        for spelling in malformed {
            assert!(
                decode_semantic_key(GoSemanticKeyRole::ProductField, spelling).is_err(),
                "accepted malformed spelling {spelling}"
            );
        }
        assert!(decode_semantic_key(GoSemanticKeyRole::ProductField, "foo").is_err());
    }

    #[test]
    fn decimal_overflow_is_rejected_in_every_decoder_role() {
        let overflow = "18446744073709551616";
        for (role, prefix) in [
            (GoSemanticKeyRole::ProductField, "F"),
            (GoSemanticKeyRole::SumArm, "K"),
        ] {
            assert!(decode_semantic_key(role, &format!("{prefix}{overflow}")).is_err());
        }
        for spelling in [
            format!("KioKey_V{overflow}_B0_"),
            format!("KioKey_V1_B{overflow}_"),
            "KioKey_V1_B18446744073709551615_".to_owned(),
            format!("KioKey_V1_Q{overflow}_N0_"),
        ] {
            assert!(
                decode_semantic_key(GoSemanticKeyRole::ProductField, &spelling).is_err(),
                "accepted overflowing spelling {spelling}"
            );
        }
    }

    #[test]
    fn facade_shell_selection_is_order_and_occupancy_independent() {
        let first = FacadeShellId::new(
            FacadeKind::Product,
            vec![bare("Left"), SemanticKey::Positional { index: 1 }],
        );
        let second = FacadeShellId::new(
            FacadeKind::Sum,
            vec![bare("Right"), SemanticKey::Positional { index: 2 }],
        );

        let forward = [
            facade_shell_go_identifier(&first),
            facade_shell_go_identifier(&second),
        ];
        let reverse = [
            facade_shell_go_identifier(&second),
            facade_shell_go_identifier(&first),
        ];
        assert_eq!(forward[0], reverse[1]);
        assert_eq!(forward[1], reverse[0]);
        assert_eq!(forward[0].as_str(), first.encode_public());
        assert_eq!(forward[1].as_str(), second.encode_public());

        let occupied =
            ScopeClaims::try_collect([("package", forward[0].clone(), "pre-existing owner")])
                .unwrap();
        assert_eq!(occupied.len(), 1);
        assert_eq!(facade_shell_go_identifier(&first), forward[0]);
    }

    #[test]
    fn repeated_same_owner_claim_is_idempotent() {
        let name = identifier("Shared");
        let claims = ScopeClaims::try_collect([
            ("package", name.clone(), "same owner"),
            ("package", name.clone(), "same owner"),
        ])
        .unwrap();

        assert_eq!(claims.len(), 1);
        assert_eq!(claims.owner(&"package", &name), Some(&"same owner"));
        assert_eq!(claims.iter().count(), 1);
        assert!(!claims.is_empty());
    }

    #[test]
    fn different_owner_claim_in_one_scope_is_a_collision() {
        let name = identifier("Shared");
        let forward = [
            ("package", name.clone(), "source declaration"),
            ("package", name.clone(), "generated facade"),
        ];
        let reverse = [forward[1].clone(), forward[0].clone()];
        let collision = ScopeClaims::try_collect(forward).unwrap_err();
        let reversed_collision = ScopeClaims::try_collect(reverse).unwrap_err();

        assert_eq!(collision, reversed_collision);
        assert_eq!(collision.scope(), &"package");
        assert_eq!(collision.name(), &name);
        assert_eq!(collision.first_owner(), &"generated facade");
        assert_eq!(collision.second_owner(), &"source declaration");
    }

    #[test]
    fn first_collision_is_canonical_across_claim_order() {
        let first_name = identifier("First");
        let second_name = identifier("Second");
        let forward = vec![
            ("scope-b", second_name.clone(), "owner-z"),
            ("scope-a", first_name.clone(), "owner-z"),
            ("scope-b", second_name, "owner-a"),
            ("scope-a", first_name.clone(), "owner-a"),
        ];
        let reverse = forward.iter().cloned().rev().collect::<Vec<_>>();

        let collision = ScopeClaims::try_collect(forward).unwrap_err();
        let reversed_collision = ScopeClaims::try_collect(reverse).unwrap_err();
        assert_eq!(collision, reversed_collision);
        assert_eq!(collision.scope(), &"scope-a");
        assert_eq!(collision.name(), &first_name);
        assert_eq!(collision.first_owner(), &"owner-a");
        assert_eq!(collision.second_owner(), &"owner-z");
    }

    #[test]
    fn same_identifier_in_different_scopes_is_allowed() {
        let name = identifier("Shared");
        let claims = ScopeClaims::try_collect([
            ("package-a", name.clone(), "owner-a"),
            ("package-b", name.clone(), "owner-b"),
        ])
        .unwrap();

        assert_eq!(claims.len(), 2);
        assert_eq!(claims.owner(&"package-a", &name), Some(&"owner-a"));
        assert_eq!(claims.owner(&"package-b", &name), Some(&"owner-b"));
    }
}
