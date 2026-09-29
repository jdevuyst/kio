//! Shared utilities for the LSP hover / goto-definition / references
//! handlers.
//!
//! The three handlers all need the same pair of operations:
//!
//! - **URI → canonical path.** Convert a `file://` URI to an absolute,
//!   canonicalized [`PathBuf`] so it matches the keys in the analysis's
//!   `file_to_module` and `sources` maps.
//!
//! - **Byte offset → smallest containing span.** Given a cursor offset
//!   and an iterator of `(Span, value)` entries from the position index,
//!   return the entry whose span contains the offset *and* is the
//!   smallest (most specific) among all such entries. The position index
//!   records all expression nodes, so the smallest containing span is
//!   the innermost expression the cursor sits on — exactly what hover
//!   and goto-definition want.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use lsp_types::{
    DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier, TextDocumentEdit, TextEdit,
    Uri, WorkspaceEdit,
};

use crate::span::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileUriPath {
    Path(PathBuf),
    NonFile,
    InvalidFile,
}

/// Decode a `file:` URI into its lexical filesystem path without resolving
/// symlinks. URI-backed syntax uses this identity to derive a module's source
/// root from its declared module path.
pub(crate) fn file_uri_path(uri: &Uri) -> FileUriPath {
    let Some(scheme) = uri.scheme() else {
        return FileUriPath::NonFile;
    };
    if !scheme.eq_lowercase("file") {
        return FileUriPath::NonFile;
    }
    if uri.query().is_some() || uri.fragment().is_some() {
        return FileUriPath::InvalidFile;
    }
    let encoded_path = uri.path().as_str();
    if contains_percent_encoded_byte(encoded_path, b'/')
        || (cfg!(windows) && contains_percent_encoded_byte(encoded_path, b'\\'))
    {
        return FileUriPath::InvalidFile;
    }
    let Ok(decoded) = uri.path().as_estr().decode().into_string() else {
        return FileUriPath::InvalidFile;
    };
    if decoded.contains('\0') {
        return FileUriPath::InvalidFile;
    }
    let mut path = if let Some(authority) = uri.authority() {
        if authority.userinfo().is_some() || authority.port().is_some() {
            return FileUriPath::InvalidFile;
        }
        let Some(host) = decode_uri_component(authority.host().as_str()) else {
            return FileUriPath::InvalidFile;
        };
        if matches!(host.as_str(), "." | "..")
            || host.chars().any(|ch| matches!(ch, '\0' | '/' | '\\'))
        {
            return FileUriPath::InvalidFile;
        }
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            if !has_valid_unc_path(&decoded) {
                return FileUriPath::InvalidFile;
            }
            PathBuf::from(format!("//{host}{decoded}"))
        } else {
            PathBuf::from(decoded.as_ref())
        }
    } else {
        PathBuf::from(decoded.as_ref())
    };
    if cfg!(windows) {
        let Some(text) = path.to_str() else {
            return FileUriPath::InvalidFile;
        };
        let bytes = text.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
            path = PathBuf::from(&text[1..]);
        }
    }
    if path.is_absolute() {
        FileUriPath::Path(path)
    } else {
        FileUriPath::InvalidFile
    }
}

fn has_valid_unc_path(decoded_path: &str) -> bool {
    let Some(path) = decoded_path.strip_prefix('/') else {
        return false;
    };
    let mut components = path.split('/').peekable();
    let share = components.next().unwrap_or_default();
    if matches!(share, "" | "." | "..") || share.contains('\\') {
        return false;
    }

    let mut depth_after_share = 0usize;
    while let Some(component) = components.next() {
        match component {
            "" if components.peek().is_none() => {}
            "" => return false,
            "." => {}
            ".." if depth_after_share == 0 => return false,
            ".." => depth_after_share -= 1,
            component if component.contains('\\') => return false,
            _ => depth_after_share += 1,
        }
    }
    true
}

fn decode_uri_component(component: &str) -> Option<String> {
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = decode_hex_digit(*bytes.get(index + 1)?)?;
            let low = decode_hex_digit(*bytes.get(index + 2)?)?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn contains_percent_encoded_byte(component: &str, expected: u8) -> bool {
    let bytes = component.as_bytes();
    bytes.windows(3).any(|window| {
        window[0] == b'%'
            && decode_hex_digit(window[1])
                .zip(decode_hex_digit(window[2]))
                .is_some_and(|(high, low)| high * 16 + low == expected)
    })
}

fn decode_hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    match file_uri_path(uri) {
        FileUriPath::Path(path) => Some(path),
        FileUriPath::NonFile | FileUriPath::InvalidFile => None,
    }
}

/// Return the decoded filename component used for Kio file-kind routing.
/// Concrete file URIs use their already-decoded lexical path; non-file
/// buffers use the decoded final URI-path component.
pub(crate) fn filename_from_uri(uri: &Uri) -> String {
    if let FileUriPath::Path(path) = file_uri_path(uri) {
        return path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
    }
    let decoded = uri
        .path()
        .as_estr()
        .decode()
        .into_string()
        .unwrap_or_else(|_| uri.path().as_str().into());
    Path::new(decoded.as_ref())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(decoded.as_ref())
        .to_owned()
}

/// Convert a `file:` URI to the canonical analysis/overlay identity. A
/// nonexistent file weak-canonicalizes its nearest existing ancestor and
/// retains the missing suffix, so unsaved and newly created editor buffers
/// share the provider identity derived below a symlinked source root.
pub fn uri_to_canonical(uri: &Uri) -> Option<PathBuf> {
    let raw = uri_to_path(uri)?;
    Some(crate::package_collection::canonicalize_with_missing_suffix(&raw).unwrap_or(raw))
}

/// Find the smallest (innermost) span among those whose byte range
/// contains `byte_offset`. Returns `(span, value)` or `None` if no
/// entry covers the offset.
///
/// "Smallest" is `end - start` — the span with the fewest bytes.
/// When multiple spans have the same size, the one with the higher
/// start offset wins (it's the rightmost, which is generally the
/// innermost for right-leaning AST shapes).
pub fn smallest_containing_span<'a, T>(
    byte_offset: u32,
    entries: impl Iterator<Item = (Span, &'a T)>,
) -> Option<(Span, &'a T)> {
    entries
        // A zero-width span covers no source text, so nothing the user can put
        // a cursor on belongs to it. Lowering passes mint generated nodes with
        // exactly such spans; answering a query from one would report a
        // compiler-internal node as if the user had written it.
        .filter(|(span, _)| span.end > span.start)
        .filter(|(span, _)| span.start <= byte_offset && byte_offset <= span.end)
        .min_by_key(|(span, _)| {
            // Prefer smallest span (innermost). Tie-break by highest start
            // (rightmost — innermost in right-leaning structures).
            let size = span.end - span.start;
            (size, u32::MAX - span.start)
        })
}

pub fn workspace_edit_from_changes(
    changes: HashMap<Uri, Vec<TextEdit>>,
    snapshot_versions: Option<&BTreeMap<Uri, i32>>,
) -> WorkspaceEdit {
    let Some(snapshot_versions) = snapshot_versions else {
        return WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        };
    };

    let edits = changes
        .into_iter()
        .map(|(uri, text_edits)| TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                version: snapshot_versions.get(&uri).copied(),
                uri,
            },
            edits: text_edits.into_iter().map(OneOf::Left).collect(),
        })
        .collect();
    WorkspaceEdit {
        document_changes: Some(DocumentChanges::Edits(edits)),
        ..Default::default()
    }
}

#[cfg(test)]
pub(crate) fn test_file_path(path: impl AsRef<Path>) -> PathBuf {
    let path = std::path::absolute(path).expect("absolute fixture path");
    crate::package_collection::canonicalize_with_missing_suffix(&path).unwrap_or(path)
}

#[cfg(test)]
pub(crate) fn test_file_uri(path: impl AsRef<Path>) -> Uri {
    let path = std::path::absolute(path).expect("absolute fixture path");
    crate::lsp::diagnostics::path_to_uri(&path, &path).expect("fixture file URI")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    struct IntVal(i32);

    fn s(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    #[test]
    fn synthetic_file_fixture_has_a_native_absolute_identity() {
        let path = test_file_path("/kio-lsp-fixtures/with space/main.kio");
        let uri = test_file_uri("/kio-lsp-fixtures/with space/main.kio");
        assert!(path.is_absolute());
        assert_eq!(uri_to_canonical(&uri), Some(path));
    }

    #[test]
    fn uri_to_path_decodes_the_complete_file_uri_path() {
        let source = if cfg!(windows) {
            "file:///C:/workspace/a%20b/%25literal/%23hash/%3Fquery/%C3%A9.kio"
        } else {
            "file:///workspace/a%20b/%25literal/%23hash/%3Fquery/%C3%A9.kio"
        };
        let uri = Uri::from_str(source).expect("file URI");
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\workspace\a b\%literal\#hash\?query\é.kio")
        } else {
            PathBuf::from("/workspace/a b/%literal/#hash/?query/é.kio")
        };
        assert_eq!(uri_to_path(&uri), Some(expected));
    }

    #[test]
    fn uri_to_path_rejects_non_file_schemes() {
        let uri = Uri::from_str("untitled:Untitled-1").expect("untitled URI");
        assert_eq!(uri_to_path(&uri), None);
    }

    #[test]
    fn uri_to_path_rejects_relative_query_fragment_and_port_forms() {
        for source in [
            "file:relative.kio",
            "file:///workspace/main.kio?version=1",
            "file:///workspace/main.kio#fragment",
            "file:///workspace/%00main.kio",
            "file:///workspace/a%2Fb/main.kio",
            "file:///workspace/a%2fb/main.kio",
            "file://server:123/share/main.kio",
        ] {
            let uri = Uri::from_str(source).expect("syntactically valid URI");
            assert_eq!(
                uri_to_path(&uri),
                None,
                "unexpected file identity for {source}"
            );
            assert_eq!(file_uri_path(&uri), FileUriPath::InvalidFile);
        }
    }

    #[cfg(windows)]
    #[test]
    fn uri_to_path_rejects_percent_encoded_windows_separators() {
        for source in [
            "file:///C:/workspace/a%5Cb/main.kio",
            "file:///C:/workspace/a%5cb/main.kio",
        ] {
            let uri = Uri::from_str(source).expect("syntactically valid URI");
            assert_eq!(file_uri_path(&uri), FileUriPath::InvalidFile);
        }
    }

    #[test]
    fn file_uri_path_distinguishes_non_file_buffers_from_invalid_file_uris() {
        let untitled = Uri::from_str("untitled:Untitled-1").expect("untitled URI");
        let invalid = Uri::from_str("file:relative.kio").expect("relative file URI");
        assert_eq!(file_uri_path(&untitled), FileUriPath::NonFile);
        assert_eq!(file_uri_path(&invalid), FileUriPath::InvalidFile);
    }

    #[test]
    fn filename_from_uri_uses_the_fully_decoded_file_or_nonfile_path() {
        let file = if cfg!(windows) {
            "file:///C:/workspace/a%20b%23%25%C3%A9.pkg.kio"
        } else {
            "file:///workspace/a%20b%23%25%C3%A9.pkg.kio"
        };
        assert_eq!(
            filename_from_uri(&Uri::from_str(file).expect("file URI")),
            "a b#%é.pkg.kio"
        );
        assert_eq!(
            filename_from_uri(
                &Uri::from_str("untitled:folder/a%20b%23%25%C3%A9.kio").expect("untitled URI")
            ),
            "a b#%é.kio"
        );
    }

    #[cfg(unix)]
    #[test]
    fn uri_to_canonical_uses_an_existing_symlink_ancestor_for_a_missing_file() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("real");
        let lexical = temp.path().join("link");
        std::fs::create_dir_all(&real).expect("real root");
        symlink(&real, &lexical).expect("root symlink");
        let missing = lexical.join("a/d/e.kio");
        let uri = crate::lsp::diagnostics::path_to_uri(&missing, temp.path())
            .expect("missing provider URI");
        let canonical_real = real.canonicalize().expect("canonical real root");
        assert_eq!(
            uri_to_canonical(&uri),
            Some(canonical_real.join("a/d/e.kio")),
            "a missing overlay file must share the provider's weak-canonical identity"
        );
    }

    #[test]
    fn uri_to_path_preserves_a_nonlocal_authority_as_unc_shape() {
        let uri = Uri::from_str("file://server/share/a%20b.kio").expect("UNC URI");
        let path = uri_to_path(&uri).expect("UNC path");
        assert!(path.to_string_lossy().contains("server"));
        assert!(path.to_string_lossy().contains("share"));
        assert!(path.to_string_lossy().contains("a b.kio"));
    }

    #[test]
    fn uri_to_path_decodes_a_percent_encoded_unc_authority() {
        let uri = Uri::from_str("file://s%C3%A9rver%20%25/share/a.kio").expect("UNC URI");
        let path = uri_to_path(&uri).expect("UNC path");
        assert!(path.to_string_lossy().contains("sérver %"));
    }

    #[test]
    fn uri_to_path_rejects_separators_encoded_in_a_unc_authority() {
        for source in [
            "file://server%2Fpeer/share/a.kio",
            "file://server%5Cpeer/share/a.kio",
            "file://server%00/share/a.kio",
            "file://./share/a.kio",
            "file://../share/a.kio",
            "file://%2E/share/a.kio",
            "file://%2e%2E/share/a.kio",
            "file://server/./a.kio",
            "file://server/../a.kio",
            "file://server/%2E/a.kio",
            "file://server/%2e%2E/a.kio",
            "file://server//share/a.kio",
            "file://server/share/../workspace/a.kio",
            "file://server/share/%2E%2E/workspace/a.kio",
            "file://server/share/child/../../workspace/a.kio",
            "file://server/share/child/%2e%2e/%2E%2E/workspace/a.kio",
            "file://server/share//a.kio",
            "file://server/",
            "file://server",
        ] {
            let uri = Uri::from_str(source).expect("syntactically valid URI");
            assert_eq!(file_uri_path(&uri), FileUriPath::InvalidFile);
        }
    }

    #[test]
    fn uri_to_path_allows_parent_steps_that_stay_below_the_unc_share() {
        let uri = Uri::from_str("file://server/share/child/../a.kio").expect("UNC URI");
        assert!(matches!(file_uri_path(&uri), FileUriPath::Path(_)));
    }

    #[cfg(windows)]
    #[test]
    fn uri_to_path_removes_the_uri_slash_before_a_drive_root() {
        let uri = Uri::from_str("file:///C:/workspace/main.kio").expect("drive URI");
        assert_eq!(
            uri_to_path(&uri),
            Some(PathBuf::from(r"C:\workspace\main.kio"))
        );
    }

    #[test]
    fn smallest_containing_span_empty_iterator() {
        let vals: Vec<(Span, &IntVal)> = vec![];
        assert!(smallest_containing_span(5, vals.into_iter()).is_none());
    }

    #[test]
    fn smallest_containing_span_none_cover_offset() {
        let v = IntVal(1);
        let entries = vec![(s(10, 20), &v), (s(30, 40), &v)];
        assert!(smallest_containing_span(5, entries.into_iter()).is_none());
    }

    #[test]
    fn smallest_containing_span_single_match() {
        let v = IntVal(42);
        let entries = vec![(s(5, 15), &v)];
        let (span, val) = smallest_containing_span(10, entries.into_iter()).unwrap();
        assert_eq!(span, s(5, 15));
        assert_eq!(val.0, 42);
    }

    #[test]
    fn smallest_containing_span_prefers_innermost() {
        let outer = IntVal(1);
        let inner = IntVal(2);
        // outer spans 0..20, inner spans 8..12; cursor at 10.
        let entries = vec![(s(0, 20), &outer), (s(8, 12), &inner)];
        let (span, val) = smallest_containing_span(10, entries.into_iter()).unwrap();
        assert_eq!(span, s(8, 12));
        assert_eq!(val.0, 2);
    }

    #[test]
    fn smallest_containing_span_at_boundary() {
        let v = IntVal(7);
        // Cursor exactly at start of span.
        let entries = vec![(s(5, 10), &v)];
        assert!(smallest_containing_span(5, entries.clone().into_iter()).is_some());
        // Cursor exactly at end of span.
        assert!(smallest_containing_span(10, entries.into_iter()).is_some());
    }

    #[test]
    fn smallest_containing_span_just_outside() {
        let v = IntVal(3);
        let entries = vec![(s(5, 10), &v)];
        assert!(smallest_containing_span(4, entries.clone().into_iter()).is_none());
        assert!(smallest_containing_span(11, entries.into_iter()).is_none());
    }
}
