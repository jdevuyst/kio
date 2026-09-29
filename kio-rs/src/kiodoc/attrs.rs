//! Attribute-list parser for Kiodoc fence info strings.
//!
//! Implements the grammar from
//! [`specs/kiodoc.md`](../../../specs/kiodoc.md) § Fence attributes:
//!
//! ```text
//! attr-list   := "{" ws* ( ref (ws+ attr)* | attr (ws+ attr)* )? ws* "}"
//! ref         := harness-ref | self-ref
//! attr        := bare-attr | kv-attr
//! bare-attr   := name
//! kv-attr     := name "=" value
//! harness-ref := "@" name
//! self-ref    := "@"          (not followed by a name — doc-comment mode)
//! name        := [a-z_] [a-z0-9_]*
//! value       := integer | name | json-string | json-object
//! integer     := [0-9]+
//! json-string := JSON string literal
//! json-object := JSON object literal
//! ws          := " " | "\t"
//! ```
//!
//! Returns a structured [`FenceAttrs`] value. The parser is purely
//! syntactic — it does not enforce per-fence vocabulary rules
//! (e.g. `harness=NAME` exclusivity, `@NAME` referring to a known
//! harness, `{@}` being a doc-comment-only form). Those are the
//! document layer's responsibility (see [`super::document`]).

use crate::span::Span;

/// Parsed attribute list for one fence. Order of bare attributes is
/// preserved for diagnostics; key-value attributes are kept in
/// source order too.
#[derive(Debug, Clone)]
pub struct FenceAttrs {
    /// `@NAME` reference (at most one per list — multi-`@` is a
    /// parse error). `None` when the list carries no `@NAME`, the bare
    /// `@` self-ref included: that form sets `self_ref` instead.
    pub harness_ref: Option<String>,
    /// `{@}` self-reference form — a bare `@` carrying no name. Only
    /// meaningful inside `///` doc-comments (the "surrounding module is
    /// the harness" form). Set when the parser reads an `@` that no name
    /// follows, in the list's first position: `{@}`,
    /// `{@ check_exit_code=14}`.
    ///
    /// Mutually exclusive with `harness_ref`: `{@}` and `{@NAME}` in
    /// the same list is a `MultiHarnessRef` parse error.
    pub self_ref: bool,
    /// Bare flags, in source order. Duplicates are rejected by the
    /// parser.
    pub bare: Vec<BareAttr>,
    /// Key-value attributes, in source order. Duplicate keys are
    /// rejected by the parser.
    pub kvs: Vec<KvAttr>,
    /// Source span the `{...}` covered inside the original markdown,
    /// for diagnostics. Set by the document layer; the parser leaves
    /// this at the default-zero span and the caller patches it.
    pub span: Span,
}

impl Default for FenceAttrs {
    fn default() -> Self {
        Self {
            harness_ref: None,
            self_ref: false,
            bare: Vec::new(),
            kvs: Vec::new(),
            span: Span::new(0, 0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BareAttr {
    pub name: String,
    /// Byte range *inside the attribute text* (not the markdown
    /// source). Caller adds the offset of the `{` to convert to a
    /// markdown-source span when reporting.
    pub local_span: Span,
}

#[derive(Debug, Clone)]
pub struct KvAttr {
    pub key: String,
    pub value: String,
    pub local_span: Span,
}

/// Structured error from the attribute parser. The `message` is a
/// short, author-facing description; `local_span` is the byte range
/// inside the attribute text that triggered it.
#[derive(Debug, Clone)]
pub struct AttrError {
    pub kind: AttrErrorKind,
    pub local_span: Span,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrErrorKind {
    /// Missing `{` at the start of the attribute string.
    MissingOpen,
    /// Missing `}` at the end of the attribute string.
    MissingClose,
    /// Trailing garbage after the closing `}`.
    TrailingGarbage,
    /// Two or more `@NAME` references in one list.
    MultiHarnessRef,
    /// A harness reference (`@NAME` or the bare `@`) that isn't the
    /// list's first attribute.
    HarnessRefNotFirst,
    /// Bare attribute repeated.
    DuplicateBare,
    /// Key-value attribute repeated.
    DuplicateKv,
    /// Unrecognized token (lexer-level error — junk character, etc.).
    BadToken,
    /// Identifier doesn't match the `[a-z_][a-z0-9_]*` rule.
    BadIdent,
    /// Value doesn't match `integer | name | json-string | json-object`.
    BadValue,
}

/// Parse the literal `{...}` attribute text. The input must include
/// the surrounding braces. Whitespace between attributes is one or
/// more spaces / tabs. Newlines inside the braces are also accepted
/// to keep the parser resilient against `\r\n`-normalized info
/// strings, though the spec grammar lists only space and tab.
pub fn parse(input: &str) -> Result<FenceAttrs, AttrError> {
    let bytes = input.as_bytes();
    let mut idx = 0usize;
    if bytes.first() != Some(&b'{') {
        return Err(AttrError {
            kind: AttrErrorKind::MissingOpen,
            local_span: Span::new(0, input.len() as u32),
            message: "attribute list must start with `{`".to_owned(),
        });
    }
    idx += 1;
    skip_ws(bytes, &mut idx);

    let mut out = FenceAttrs::default();
    let mut first = true;
    loop {
        if idx >= bytes.len() {
            return Err(AttrError {
                kind: AttrErrorKind::MissingClose,
                local_span: Span::new(idx as u32, idx as u32),
                message: "attribute list must end with `}`".to_owned(),
            });
        }
        if bytes[idx] == b'}' {
            idx += 1;
            break;
        }
        let start = idx;
        let b = bytes[idx];
        match b {
            b'@' => {
                idx += 1;
                // A bare `@` — one immediately followed by the list's
                // `}`, or by the whitespace separating it from the next
                // attribute — is the doc-comment self-ref form (`{@}`,
                // `{@ check_exit_code=14}`).
                //
                // Any other byte belongs to a `harness-ref`'s name, so
                // it goes to `parse_ident` — which reports a precise
                // `BadIdent` for a name that doesn't start `[a-z_]`
                // (`{@Main}`), rather than a vaguer separator error.
                let ends_attr = match bytes.get(idx) {
                    Some(c) => matches!(c, b'}' | b' ' | b'\t' | b'\r' | b'\n'),
                    None => true,
                };
                // `None` is the bare `@`. Consuming the name here leaves
                // `idx` just past the whole attribute either way: for the
                // self-ref that is the byte after the `@`, which the
                // post-match whitespace check reads as the separator
                // before `}` or the next attribute.
                let name = if ends_attr {
                    None
                } else {
                    let (name, name_end) = parse_ident(bytes, idx)?;
                    idx = name_end;
                    Some(name)
                };
                // A second reference is a multi-harness error wherever it
                // sits, and saying so reads better than blaming its
                // position — so this check precedes the `first` one.
                if out.harness_ref.is_some() || out.self_ref {
                    return Err(AttrError {
                        kind: AttrErrorKind::MultiHarnessRef,
                        local_span: Span::new(start as u32, idx as u32),
                        message: "at most one `@` / `@NAME` attribute per fence".to_owned(),
                    });
                }
                if !first {
                    return Err(AttrError {
                        kind: AttrErrorKind::HarnessRefNotFirst,
                        local_span: Span::new(start as u32, idx as u32),
                        message: "the harness reference must be a fence's first \
                                  attribute — move it before the others"
                            .to_owned(),
                    });
                }
                match name {
                    Some(name) => out.harness_ref = Some(name),
                    None => out.self_ref = true,
                }
            }
            b'a'..=b'z' | b'_' => {
                let (key, key_end) = parse_ident(bytes, idx)?;
                idx = key_end;
                if idx < bytes.len() && bytes[idx] == b'=' {
                    idx += 1;
                    let (value, value_end) = parse_value(bytes, idx)?;
                    idx = value_end;
                    if out.kvs.iter().any(|kv| kv.key == key) {
                        return Err(AttrError {
                            kind: AttrErrorKind::DuplicateKv,
                            local_span: Span::new(start as u32, idx as u32),
                            message: format!("attribute `{key}` is set more than once"),
                        });
                    }
                    out.kvs.push(KvAttr {
                        key,
                        value,
                        local_span: Span::new(start as u32, idx as u32),
                    });
                } else {
                    if out.bare.iter().any(|b| b.name == key) {
                        return Err(AttrError {
                            kind: AttrErrorKind::DuplicateBare,
                            local_span: Span::new(start as u32, idx as u32),
                            message: format!("attribute `{key}` appears more than once"),
                        });
                    }
                    out.bare.push(BareAttr {
                        name: key,
                        local_span: Span::new(start as u32, idx as u32),
                    });
                }
            }
            _ => {
                return Err(AttrError {
                    kind: AttrErrorKind::BadToken,
                    local_span: Span::new(start as u32, (start + 1) as u32),
                    message: format!("unexpected character `{}` in attribute list", char::from(b)),
                });
            }
        }
        // Require whitespace before the next attribute, or `}`.
        first = false;
        let before_ws = idx;
        skip_ws(bytes, &mut idx);
        if idx == before_ws && idx < bytes.len() && bytes[idx] != b'}' {
            return Err(AttrError {
                kind: AttrErrorKind::BadToken,
                local_span: Span::new(idx as u32, (idx + 1) as u32),
                message: "expected whitespace or `}` after attribute".to_owned(),
            });
        }
    }
    if idx < bytes.len() {
        return Err(AttrError {
            kind: AttrErrorKind::TrailingGarbage,
            local_span: Span::new(idx as u32, bytes.len() as u32),
            message: "unexpected text after `}` in attribute list".to_owned(),
        });
    }
    Ok(out)
}

/// Eat space, tab, CR, and LF characters. The grammar lists only
/// space/tab, but tolerating CR/LF makes the parser resilient when
/// an info string is split across lines (rare; CommonMark forbids
/// this on visible fences, but the HTML-comment opener can wrap).
fn skip_ws(bytes: &[u8], idx: &mut usize) {
    while *idx < bytes.len() {
        match bytes[*idx] {
            b' ' | b'\t' | b'\r' | b'\n' => *idx += 1,
            _ => return,
        }
    }
}

/// Parse `[a-z_][a-z0-9_]*` starting at `idx`. Returns `(name, end)`.
fn parse_ident(bytes: &[u8], idx: usize) -> Result<(String, usize), AttrError> {
    if idx >= bytes.len() {
        return Err(AttrError {
            kind: AttrErrorKind::BadIdent,
            local_span: Span::new(idx as u32, idx as u32),
            message: "expected identifier".to_owned(),
        });
    }
    let first = bytes[idx];
    if !matches!(first, b'a'..=b'z' | b'_') {
        return Err(AttrError {
            kind: AttrErrorKind::BadIdent,
            local_span: Span::new(idx as u32, (idx + 1) as u32),
            message: format!(
                "identifier must start with [a-z_], got `{}`",
                char::from(first)
            ),
        });
    }
    let mut end = idx + 1;
    while end < bytes.len() {
        let b = bytes[end];
        if matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_') {
            end += 1;
        } else {
            break;
        }
    }
    let name = std::str::from_utf8(&bytes[idx..end])
        .expect("identifier bytes are ASCII by construction")
        .to_owned();
    Ok((name, end))
}

/// Parse a value: `[0-9]+`, `[a-z_][a-z0-9_]*`, a JSON string, or a JSON object.
fn parse_value(bytes: &[u8], idx: usize) -> Result<(String, usize), AttrError> {
    if idx >= bytes.len() {
        return Err(AttrError {
            kind: AttrErrorKind::BadValue,
            local_span: Span::new(idx as u32, idx as u32),
            message: "expected value after `=`".to_owned(),
        });
    }
    let first = bytes[idx];
    if first.is_ascii_digit() {
        let mut end = idx + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let value = std::str::from_utf8(&bytes[idx..end])
            .expect("digits are ASCII")
            .to_owned();
        Ok((value, end))
    } else if matches!(first, b'a'..=b'z' | b'_') {
        let (name, end) = parse_ident(bytes, idx)?;
        Ok((name, end))
    } else if first == b'"' || first == b'{' {
        parse_json_value(bytes, idx)
    } else {
        Err(AttrError {
            kind: AttrErrorKind::BadValue,
            local_span: Span::new(idx as u32, (idx + 1) as u32),
            message: format!(
                "value must be an integer, identifier, JSON string, or JSON object, got `{}`",
                char::from(first)
            ),
        })
    }
}

fn parse_json_value(bytes: &[u8], idx: usize) -> Result<(String, usize), AttrError> {
    match bytes[idx] {
        b'"' => parse_json_string(bytes, idx),
        b'{' => parse_json_object(bytes, idx),
        other => Err(AttrError {
            kind: AttrErrorKind::BadValue,
            local_span: Span::new(idx as u32, (idx + 1) as u32),
            message: format!(
                "expected JSON string or object, got `{}`",
                char::from(other)
            ),
        }),
    }
}

fn parse_json_string(bytes: &[u8], idx: usize) -> Result<(String, usize), AttrError> {
    let end = scan_json_string(bytes, idx)?;
    let value = std::str::from_utf8(&bytes[idx..end])
        .expect("JSON literal slice is valid UTF-8 by source construction")
        .to_owned();
    Ok((value, end))
}

fn parse_json_object(bytes: &[u8], idx: usize) -> Result<(String, usize), AttrError> {
    let mut end = idx + 1;
    let mut depth = 1usize;
    while end < bytes.len() {
        match bytes[end] {
            b'"' => end = scan_json_string(bytes, end)?,
            b'{' => {
                depth += 1;
                end += 1;
            }
            b'}' => {
                depth -= 1;
                end += 1;
                if depth == 0 {
                    let value = std::str::from_utf8(&bytes[idx..end])
                        .expect("JSON literal slice is valid UTF-8 by source construction")
                        .to_owned();
                    return Ok((value, end));
                }
            }
            _ => end += 1,
        }
    }
    Err(AttrError {
        kind: AttrErrorKind::BadValue,
        local_span: Span::new(idx as u32, bytes.len() as u32),
        message: "JSON object value must end with `}`".to_owned(),
    })
}

fn scan_json_string(bytes: &[u8], idx: usize) -> Result<usize, AttrError> {
    debug_assert_eq!(bytes[idx], b'"');
    let mut end = idx + 1;
    while end < bytes.len() {
        match bytes[end] {
            b'"' => return Ok(end + 1),
            b'\\' => {
                end += 1;
                if end >= bytes.len() {
                    return Err(AttrError {
                        kind: AttrErrorKind::BadValue,
                        local_span: Span::new(idx as u32, end as u32),
                        message: "JSON string has a trailing escape".to_owned(),
                    });
                }
                end += 1;
            }
            b'\r' | b'\n' => {
                return Err(AttrError {
                    kind: AttrErrorKind::BadValue,
                    local_span: Span::new(end as u32, (end + 1) as u32),
                    message: "JSON string value may not contain a newline".to_owned(),
                });
            }
            _ => end += 1,
        }
    }
    Err(AttrError {
        kind: AttrErrorKind::BadValue,
        local_span: Span::new(idx as u32, bytes.len() as u32),
        message: "JSON string value must end with `\"`".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_braces_parse() {
        let a = parse("{}").unwrap();
        assert!(a.harness_ref.is_none());
        assert!(a.bare.is_empty());
        assert!(a.kvs.is_empty());
    }

    #[test]
    fn single_bare_attribute() {
        let a = parse("{ignore}").unwrap();
        assert_eq!(a.bare.len(), 1);
        assert_eq!(a.bare[0].name, "ignore");
    }

    #[test]
    fn json_string_key_value_attribute() {
        let a = parse("{harness=main placeholder=\"__SNIPPET__\"}").unwrap();
        assert_eq!(a.kvs.len(), 2);
        assert_eq!(a.kvs[0].key, "harness");
        assert_eq!(a.kvs[0].value, "main");
        assert_eq!(a.kvs[1].key, "placeholder");
        assert_eq!(a.kvs[1].value, "\"__SNIPPET__\"");
    }

    #[test]
    fn json_object_key_value_attribute() {
        let a =
            parse("{@main placeholder={\"...\":\"dummy_code()\",\"???\":\"fallback()\"}}").unwrap();
        assert_eq!(a.kvs.len(), 1);
        assert_eq!(a.kvs[0].key, "placeholder");
        assert_eq!(
            a.kvs[0].value,
            "{\"...\":\"dummy_code()\",\"???\":\"fallback()\"}"
        );
    }

    #[test]
    fn single_harness_ref() {
        let a = parse("{@main}").unwrap();
        assert_eq!(a.harness_ref.as_deref(), Some("main"));
        assert!(!a.self_ref);
    }

    #[test]
    fn self_ref_bare_at() {
        let a = parse("{@}").unwrap();
        assert!(a.harness_ref.is_none());
        assert!(a.self_ref);
        assert!(a.bare.is_empty());
        assert!(a.kvs.is_empty());
    }

    #[test]
    fn self_ref_before_bare_attr() {
        let a = parse("{@ stdout}").unwrap();
        assert!(a.self_ref);
        assert!(a.harness_ref.is_none());
        assert_eq!(a.bare.len(), 1);
        assert_eq!(a.bare[0].name, "stdout");
    }

    #[test]
    fn self_ref_before_kv_attr() {
        let a = parse("{@ check_exit_code=14}").unwrap();
        assert!(a.self_ref);
        assert_eq!(a.kvs.len(), 1);
        assert_eq!(a.kvs[0].key, "check_exit_code");
        assert_eq!(a.kvs[0].value, "14");
    }

    #[test]
    fn self_ref_before_several_attrs() {
        let a = parse("{@ check_exit_code=14 stdout}").unwrap();
        assert!(a.self_ref);
        assert!(a.harness_ref.is_none());
        assert_eq!(a.kvs.len(), 1);
        assert_eq!(a.kvs[0].value, "14");
        assert_eq!(a.bare.len(), 1);
        assert_eq!(a.bare[0].name, "stdout");
    }

    /// A harness reference opens the list. Flags and key-value
    /// attributes are order-free among themselves, but neither form of
    /// reference may follow one.
    #[test]
    fn harness_ref_must_be_first() {
        for input in [
            "{check_exit_code=14 @}",
            "{stdout @}",
            "{check_exit_code=14 @main}",
            "{stdout @main}",
        ] {
            assert_eq!(
                parse(input).unwrap_err().kind,
                AttrErrorKind::HarnessRefNotFirst,
                "{input} should reject a trailing harness reference"
            );
        }
    }

    /// Two references report the duplication rather than the position,
    /// even though the second one is also not first.
    #[test]
    fn multi_harness_ref_outranks_not_first() {
        for input in ["{@main @}", "{@ @main}", "{@ @}", "{@main @other}"] {
            assert_eq!(
                parse(input).unwrap_err().kind,
                AttrErrorKind::MultiHarnessRef,
                "{input} should report the duplicate reference"
            );
        }
    }

    /// A bare `@` running straight to end-of-input (no closing `}`) is
    /// still a self-ref; the missing brace is the outer loop's error.
    #[test]
    fn self_ref_at_end_of_input_is_missing_close() {
        assert_eq!(parse("{@").unwrap_err().kind, AttrErrorKind::MissingClose);
    }

    /// `@` immediately followed by a non-name byte still routes to the
    /// ident parser, so a malformed `@NAME` keeps its precise
    /// `BadIdent` rather than degrading to a separator error.
    #[test]
    fn named_ref_with_bad_name_is_bad_ident() {
        assert_eq!(parse("{@Main}").unwrap_err().kind, AttrErrorKind::BadIdent);
        assert_eq!(parse("{@1}").unwrap_err().kind, AttrErrorKind::BadIdent);
    }

    #[test]
    fn single_kv_attribute() {
        let a = parse("{check_exit_code=14}").unwrap();
        assert_eq!(a.kvs.len(), 1);
        assert_eq!(a.kvs[0].key, "check_exit_code");
        assert_eq!(a.kvs[0].value, "14");
    }

    #[test]
    fn multiple_attrs_mixed() {
        let a = parse("{@main stdout check_exit_code=14}").unwrap();
        assert_eq!(a.harness_ref.as_deref(), Some("main"));
        assert_eq!(a.bare.len(), 1);
        assert_eq!(a.bare[0].name, "stdout");
        assert_eq!(a.kvs.len(), 1);
        assert_eq!(a.kvs[0].key, "check_exit_code");
    }

    #[test]
    fn order_irrelevant() {
        // Flags and key-value attributes carry no order among
        // themselves. The harness reference is the exception — it opens
        // the list; see `harness_ref_must_be_first`.
        let a = parse("{@main stdout check_exit_code=14}").unwrap();
        let b = parse("{@main check_exit_code=14 stdout}").unwrap();
        for x in [&a, &b] {
            assert_eq!(x.harness_ref.as_deref(), Some("main"));
            assert_eq!(x.kvs.len(), 1);
            assert_eq!(x.kvs[0].value, "14");
            assert_eq!(x.bare.len(), 1);
            assert_eq!(x.bare[0].name, "stdout");
        }
    }

    #[test]
    fn whitespace_tolerated() {
        let a = parse("{  @main   stdout  }").unwrap();
        assert_eq!(a.harness_ref.as_deref(), Some("main"));
        assert_eq!(a.bare.len(), 1);
    }

    #[test]
    fn missing_open_brace_errors() {
        assert_eq!(
            parse("@main}").unwrap_err().kind,
            AttrErrorKind::MissingOpen
        );
    }

    #[test]
    fn missing_close_brace_errors() {
        assert_eq!(
            parse("{@main").unwrap_err().kind,
            AttrErrorKind::MissingClose
        );
    }

    #[test]
    fn multi_harness_ref_errors() {
        assert_eq!(
            parse("{@main @other}").unwrap_err().kind,
            AttrErrorKind::MultiHarnessRef
        );
    }

    #[test]
    fn duplicate_bare_errors() {
        assert_eq!(
            parse("{stdout stdout}").unwrap_err().kind,
            AttrErrorKind::DuplicateBare
        );
    }

    #[test]
    fn duplicate_kv_errors() {
        assert_eq!(
            parse("{harness=a harness=b}").unwrap_err().kind,
            AttrErrorKind::DuplicateKv
        );
    }

    #[test]
    fn bad_token_errors() {
        assert!(matches!(
            parse("{@main; stdout}").unwrap_err().kind,
            AttrErrorKind::BadToken
        ));
    }

    #[test]
    fn uppercase_ident_errors() {
        assert_eq!(parse("{@Main}").unwrap_err().kind, AttrErrorKind::BadIdent);
    }

    #[test]
    fn missing_whitespace_between_attrs_errors() {
        // After consuming `@main` the parser expects whitespace or
        // `}` — the bare `@other` next door fails the post-attr
        // whitespace check first.
        assert!(matches!(
            parse("{@main@other}").unwrap_err().kind,
            AttrErrorKind::BadToken
        ));
    }

    #[test]
    fn unterminated_json_string_value_errors() {
        assert_eq!(
            parse("{harness=\"x}").unwrap_err().kind,
            AttrErrorKind::BadValue
        );
    }
}
