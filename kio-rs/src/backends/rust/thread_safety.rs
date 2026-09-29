//! Rust backend — the `thread_safety` build-block key.
//!
//! By default the Rust emitter wraps every fn-value / existential
//! slot in `Rc<dyn …>` and bounds host items / type-parameters with
//! `Clone + 'static` (associated types additionally `PartialEq` and, for a
//! role-bearing declaration, its standard `From<primitive>` capability).
//! `Rc` is single-threaded: an emitted package value cannot cross a
//! thread boundary. The `thread_safety` build-block key opts a crate
//! into the thread-safe `Arc<dyn …>` shape with the matching marker
//! bounds. Each accepted opt-in value (`send`, `sync`, or `send_sync`)
//! selects that full `Send + Sync` representation, which permits package
//! values to be moved between threads, shared between threads, or both.
//!
//! The opt-in is realized as a **single post-emit transform** over
//! the emitted crate's source strings ([`ThreadSafety::transform_crate`]).
//! The transform is the *identity* for [`ThreadSafety::default`]. For an
//! opt-in value it performs two uniform edits over the emitted Rust:
//!
//! 1. **Wrap shape** — `Rc` → `Arc` everywhere the emitter spells the
//!    reference-counted wrapper (`::std::rc::Rc` / `std::rc::Rc` /
//!    `Rc::new` / `Rc::ptr_eq` / `use std::rc::Rc`).
//! 2. **Marker bounds** — every emitted `dyn` trait object gains `+ Send
//!    + Sync`, and every emitted lifetime / `Clone` bound gains the same
//!    markers so the wrapped values satisfy the `Arc`-flavored bounds.
//!
//! The emitter only ever spells a `dyn` trait object as the direct
//! child of an `Rc<…>` wrapper (`Rc<dyn Fn(…) -> …>` or
//! `Rc<dyn Any>`), so the marker suffix can be placed reliably by
//! scanning to the matching `>` of the enclosing `Arc<` after the
//! wrap-shape swap. See [`insert_dyn_markers`].
//!
//! The runtime-support file (`src/__kio_runtime.rs`) is transformed from the
//! same canonical source by [`runtime_arc`]. The shared lifetime-bound pass
//! covers its `as_any<T: 'static>` payload as well as generated marker,
//! callable, forall, and host-storage bounds.
//!
//! See [`specs/package.md` § Per-target keys](../../../../../specs/package.md)
//! and [`specs/backends/rust.md` § Output layout](../../../../../specs/backends/rust.md).

use super::emit::RustCrate;

/// The thread-safety setting for an emitted Rust crate. Selected by
/// the `thread_safety` build-block key; absent ⇒ [`ThreadSafety::RcLocal`]
/// (the single-threaded default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadSafety {
    /// No key set. `Rc<dyn …>` wrap, `Clone + 'static` bounds. The
    /// emitted package value is single-threaded; the transform is the
    /// identity.
    #[default]
    RcLocal,
    /// The `thread_safety = "send"` opt-in. Uses the shared
    /// `Arc<dyn … + Send + Sync>` representation; a host may move
    /// package values between threads.
    Send,
    /// The `thread_safety = "sync"` opt-in. Uses the shared
    /// `Arc<dyn … + Send + Sync>` representation; a host may share
    /// package values between threads.
    Sync,
    /// The `thread_safety = "send_sync"` opt-in. Uses the shared
    /// `Arc<dyn … + Send + Sync>` representation; a host may both move
    /// and share package values between threads.
    SendSync,
}

impl ThreadSafety {
    /// Parse the `thread_safety` build-block value. The value is one of
    /// the three opt-in tokens; anything else is an **input error**
    /// (bad build-block value) the caller surfaces with a span. The
    /// absent-key case is the caller's responsibility (it never calls
    /// this) and maps to [`ThreadSafety::RcLocal`].
    ///
    /// The accepted spellings are the build-block string values
    /// (`"send"` / `"sync"` / `"send_sync"`); the build file's
    /// directive-style syntax delivers them already unquoted.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "send" => Ok(ThreadSafety::Send),
            "sync" => Ok(ThreadSafety::Sync),
            "send_sync" => Ok(ThreadSafety::SendSync),
            other => Err(format!(
                "unknown value `{other}` for the `thread_safety` key on the `rust` backend \
                 (recognized: `send`, `sync`, `send_sync`)"
            )),
        }
    }

    /// The marker bound list this setting adds, as the `+`-joined
    /// suffix that follows a `dyn` trait object / a `Clone` / a
    /// lifetime bound. Empty string for the default (no markers).
    ///
    /// Every opt-in value yields the same `+ Send + Sync` suffix. The
    /// resulting representation satisfies the `send`, `sync`, and
    /// `send_sync` selections and gives each selection the same bounds
    /// on wrapped contents, including captures and erased type parameters.
    fn marker_suffix(self) -> &'static str {
        match self {
            ThreadSafety::RcLocal => "",
            // All accepted opt-ins select the shared full marker set.
            ThreadSafety::Send | ThreadSafety::Sync | ThreadSafety::SendSync => " + Send + Sync",
        }
    }

    /// True for any opt-in (non-default) setting.
    fn opts_in(self) -> bool {
        !matches!(self, ThreadSafety::RcLocal)
    }

    /// Rewrite an emitted [`RustCrate`]'s source strings in place to
    /// the `Arc`-flavored, marker-bounded shape this setting selects.
    /// The default ([`ThreadSafety::RcLocal`]) is a no-op — the crate
    /// is left byte-identical.
    ///
    /// The Rust source fields (`lib_rs`, `host_rs`, `shapes_rs`,
    /// `ffi_rs`, `runtime_rs`) get the wrap-swap + marker transform.
    /// `Cargo.toml` carries no `Rc` / `dyn` text and is untouched.
    pub fn transform_crate(self, krate: &mut RustCrate) {
        if !self.opts_in() {
            return;
        }
        krate.lib_rs = transform_emitted_source(&krate.lib_rs, self);
        krate.host_rs = transform_emitted_source(&krate.host_rs, self);
        krate.shapes_rs = transform_emitted_source(&krate.shapes_rs, self);
        // FFI aliases name canonical carriers and host-selected storage
        // verbatim, so their nested reference-counted support must transform
        // in lockstep with `shapes.rs`.
        krate.ffi_rs = transform_emitted_source(&krate.ffi_rs, self);
        // Render the runtime's Arc flavor from its canonical embedded source.
        krate.runtime_rs = runtime_arc(self);
    }
}

/// Apply the wrap-shape swap and the marker-bound insertions to one
/// emitted Rust source string (the body of `lib.rs` / `host.rs` /
/// `shapes.rs`). Order matters: swap `Rc` → `Arc` first so the
/// `dyn`-marker scanner sees `Arc<dyn …>` wrappers, then add the
/// `dyn` markers, then add the lifetime / `Clone` bound markers.
fn transform_emitted_source(src: &str, ts: ThreadSafety) -> String {
    if !ts.opts_in() {
        return src.to_owned();
    }
    let swapped = swap_rc_to_arc(src);
    let with_dyn = insert_dyn_markers(&swapped, ts.marker_suffix());
    insert_bound_markers(&with_dyn, ts.marker_suffix())
}

/// Swap every emitted Rust-code spelling of the `Rc` reference-counted wrapper
/// for its `Arc` counterpart. The emitter spells `Rc` four ways in
/// generated source:
///
/// - `::std::rc::Rc` — the fully-qualified path the body / shape /
///   trait emit uses at type and `::new` positions.
/// - `std::rc::Rc` — the same path without the leading `::` (the
///   runtime file's `use std::rc::Rc;` — but the runtime file is
///   rendered separately, so this is belt-and-suspenders).
/// - `Rc::new` / `Rc::ptr_eq` / bare `Rc<` — short forms after a
///   `use std::rc::Rc;`.
///
/// Replacing the fully-qualified path covers every emit.rs site; the
/// short-form replacements are harmless on emit.rs output (it uses no
/// short `Rc`) and keep the function reusable for any future short-form
/// emit.
fn swap_rc_to_arc(src: &str) -> String {
    rewrite_rust_code_spans(src, swap_rc_to_arc_in_code)
}

fn swap_rc_to_arc_in_code(src: &str) -> String {
    src.replace("::std::rc::Rc", "::std::sync::Arc")
        .replace("std::rc::Rc", "std::sync::Arc")
        .replace("Rc::new", "Arc::new")
        .replace("Rc::ptr_eq", "Arc::ptr_eq")
        .replace("Rc<", "Arc<")
}

/// Insert the marker suffix at the end of every emitted code-position `dyn` trait
/// object. The emitter only ever spells a `dyn` trait object as the
/// direct child of a (now-swapped) `Arc<…>` wrapper —
/// `Arc<dyn Fn(…) -> …>` and `Arc<dyn ::std::any::Any>` (and the
/// runtime file's `Arc<dyn Any>` short form). So for each `Arc<dyn`
/// occurrence we find the matching `>` that closes that `Arc<` and
/// splice the marker in just before it.
///
/// Finding the matching close handles nesting correctly: an
/// `Arc<dyn Fn(Arc<dyn Any>) -> Arc<dyn Any>>` return type contains
/// nested `Arc<…>`s, and the depth counter walks past them to the
/// outer wrapper's close. Each nested `Arc<dyn …>` is itself an
/// `Arc<dyn` occurrence later in the scan, so it gets its own marker.
///
/// `<` / `>` are the only bracket kind that matters for Rust generic
/// nesting here; `(` / `)` in a `Fn(…)` parameter list never contain
/// an unbalanced `<` / `>` (every `<` inside is part of a fully
/// bracketed generic), so the angle-bracket depth counter alone finds
/// the right close.
fn insert_dyn_markers(src: &str, marker: &str) -> String {
    if marker.is_empty() {
        return src.to_owned();
    }
    rewrite_rust_code_spans(src, |code| insert_dyn_markers_in_code(code, marker))
}

fn insert_dyn_markers_in_code(src: &str, marker: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 64);
    let mut i = 0;
    // Byte positions at which a marker must be spliced (just before
    // the `>` at that index). Collected in a set-like sorted vec; the
    // scan emits markers as it reaches each close.
    let needle = b"Arc<dyn";
    // For each `Arc<dyn` match, compute the index of its closing `>`
    // and record it.
    let mut marker_before: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Position of the `<` that opens this wrapper.
            let open = i + 3; // `Arc` is 3 bytes; `<` follows.
            debug_assert_eq!(bytes[open], b'<');
            if let Some(close) = matching_angle(bytes, open) {
                marker_before.insert(close);
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    let mut copied_through = 0;
    for close in marker_before {
        out.push_str(&src[copied_through..close]);
        out.push_str(marker);
        copied_through = close;
    }
    out.push_str(&src[copied_through..]);
    out
}

/// Given the index of an opening `<` in `bytes`, return the index of
/// its matching `>`, or `None` if unbalanced (which would be an
/// emitter bug — the input is always well-bracketed Rust).
///
/// The `>` in a `->` return arrow is **not** a closing angle bracket
/// and must not decrement the depth — a `dyn Fn(…) -> R` return type
/// inside the wrapper would otherwise mis-close at the arrow. We skip
/// any `>` immediately preceded by `-`.
fn matching_angle(bytes: &[u8], open: usize) -> Option<usize> {
    debug_assert_eq!(bytes[open], b'<');
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' if i > 0 && bytes[i - 1] == b'-' => {
                // Part of a `->` arrow, not a generic close.
            }
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Insert the marker suffix immediately before every emitted `'static`
/// bound. All generated lifetime bounds have one of two grammar shapes:
/// `… + 'static` or `T: 'static`; the emitter never generates a lifetime
/// parameter bound such as `'a: 'static`. Covering the grammar rather than a
/// list of trait prefixes keeps marker/facade support coherent: it marks
/// native storage, `KioType`/constructor families, KioFn ingress closures,
/// site-owned forall traits and constructors, and host DStorage uniformly.
/// Only Rust code spans are rewritten: quoted and raw strings plus line and
/// nested block comments are copied byte-for-byte, so Kio literal values and
/// generated documentation cannot acquire marker text by coincidence.
///
/// The marker is always `Send + Sync` (without a leading `+ ` —
/// [`ThreadSafety::marker_suffix`] includes the leading `+ `, which we strip
/// here since these splices sit mid-list).
fn insert_bound_markers(src: &str, marker_suffix: &str) -> String {
    if marker_suffix.is_empty() {
        return src.to_owned();
    }
    // `marker_suffix` is e.g. " + Send + Sync"; the mid-list form is
    // "Send + Sync".
    let mid = marker_suffix.trim_start_matches(" + ");
    rewrite_rust_code_spans(src, |code| {
        code.replace(" + 'static", &format!(" + {mid} + 'static"))
            .replace(": 'static", &format!(": {mid} + 'static"))
    })
}

/// Rewrite only code spans in the generated Rust source. The emitter uses
/// ordinary quoted strings, Rust raw strings, and comments; those protected
/// spans may contain arbitrary Kio literal bytes or explanatory Rust-looking
/// examples and therefore cannot participate in a syntax transform.
fn rewrite_rust_code_spans(src: &str, mut rewrite: impl FnMut(&str) -> String) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut code_start = 0usize;
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let protected_end = if bytes[cursor..].starts_with(b"//") {
            Some(
                bytes[cursor..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| cursor + offset),
            )
        } else if bytes[cursor..].starts_with(b"/*") {
            Some(nested_block_comment_end(bytes, cursor))
        } else if bytes[cursor] == b'r' {
            raw_string_end(bytes, cursor)
        } else if bytes[cursor] == b'"' {
            Some(quoted_string_end(bytes, cursor))
        } else {
            None
        };
        let Some(protected_end) = protected_end else {
            cursor += 1;
            continue;
        };
        out.push_str(&rewrite(&src[code_start..cursor]));
        out.push_str(&src[cursor..protected_end]);
        cursor = protected_end;
        code_start = protected_end;
    }
    out.push_str(&rewrite(&src[code_start..]));
    out
}

fn quoted_string_end(bytes: &[u8], start: usize) -> usize {
    debug_assert_eq!(bytes[start], b'"');
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            b'"' => return cursor + 1,
            _ => cursor += 1,
        }
    }
    bytes.len()
}

/// Return the end of a raw string beginning at `r`, including the closing
/// quote and hashes. A preceding byte-string prefix is left in the code span;
/// the `r#*"…"#*` portion itself is still copied without inspection.
fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(bytes[start], b'r');
    let mut opening_quote = start + 1;
    while opening_quote < bytes.len() && bytes[opening_quote] == b'#' {
        opening_quote += 1;
    }
    if bytes.get(opening_quote) != Some(&b'"') {
        return None;
    }
    let hashes = opening_quote - start - 1;
    let mut cursor = opening_quote + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && cursor + 1 + hashes <= bytes.len()
            && bytes[cursor + 1..cursor + 1 + hashes]
                .iter()
                .all(|byte| *byte == b'#')
        {
            return Some(cursor + 1 + hashes);
        }
        cursor += 1;
    }
    Some(bytes.len())
}

fn nested_block_comment_end(bytes: &[u8], start: usize) -> usize {
    debug_assert!(bytes[start..].starts_with(b"/*"));
    let mut depth = 1usize;
    let mut cursor = start + 2;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"/*") {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor..].starts_with(b"*/") {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return cursor;
            }
        } else {
            cursor += 1;
        }
    }
    bytes.len()
}

/// Render the `Arc`-flavored runtime-support file for an opt-in
/// setting. Produced from the canonical `Rc` source by swapping the
/// wrap shape, adding the `dyn` markers, and marking the bound forms
/// the runtime helpers carry so the produced `Arc<dyn Any + markers>`
/// is well-formed.
///
/// The runtime source uses the same two `'static` grammar shapes as emitted
/// crate source, so the shared bound transform covers both `as_any` and
/// `from_any` without a runtime-only exception.
fn runtime_arc(ts: ThreadSafety) -> String {
    let marker = ts.marker_suffix();
    let src = super::RUNTIME_SUPPORT_FILE_CONTENT;
    // 1. Wrap shape + `dyn` markers (shared with emitted files).
    let s = swap_rc_to_arc(src);
    let s = insert_dyn_markers(&s, marker);
    // 2. Bound forms shared with emitted files.
    insert_bound_markers(&s, marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_three_opt_ins() {
        assert_eq!(ThreadSafety::parse("send"), Ok(ThreadSafety::Send));
        assert_eq!(ThreadSafety::parse("sync"), Ok(ThreadSafety::Sync));
        assert_eq!(ThreadSafety::parse("send_sync"), Ok(ThreadSafety::SendSync));
    }

    #[test]
    fn parse_rejects_unknown_values() {
        assert!(ThreadSafety::parse("Send").is_err());
        assert!(ThreadSafety::parse("threadsafe").is_err());
        assert!(ThreadSafety::parse("").is_err());
    }

    #[test]
    fn default_is_rc_local() {
        assert_eq!(ThreadSafety::default(), ThreadSafety::RcLocal);
    }

    #[test]
    fn rc_local_transform_is_identity() {
        let s = "::std::rc::Rc<dyn Fn(::std::rc::Rc<dyn ::std::any::Any>) -> .>";
        assert_eq!(transform_emitted_source(s, ThreadSafety::RcLocal), s);
    }

    #[test]
    fn opt_in_swaps_rc_and_marks_dyn() {
        let s = "::std::rc::Rc<dyn ::std::any::Any>";
        let got = transform_emitted_source(s, ThreadSafety::SendSync);
        assert_eq!(got, "::std::sync::Arc<dyn ::std::any::Any + Send + Sync>");
        // All three opt-ins select the same representation.
        assert_eq!(transform_emitted_source(s, ThreadSafety::Send), got);
        assert_eq!(transform_emitted_source(s, ThreadSafety::Sync), got);
    }

    #[test]
    fn nested_dyn_each_gets_its_own_marker() {
        let s = "::std::rc::Rc<dyn Fn(::std::rc::Rc<dyn ::std::any::Any>) \
                 -> ::std::rc::Rc<dyn ::std::any::Any>>";
        // Every opt-in selects `Send + Sync` (see `marker_suffix`).
        let got = transform_emitted_source(s, ThreadSafety::Send);
        // Each of the three `Arc<dyn …>` wrappers gets `+ Send + Sync`
        // before its own close.
        assert_eq!(
            got,
            "::std::sync::Arc<dyn Fn(::std::sync::Arc<dyn ::std::any::Any + Send + Sync>) \
             -> ::std::sync::Arc<dyn ::std::any::Any + Send + Sync> + Send + Sync>"
        );
        // `sync` and `send_sync` select the same output.
        assert_eq!(transform_emitted_source(s, ThreadSafety::Sync), got);
        assert_eq!(transform_emitted_source(s, ThreadSafety::SendSync), got);
    }

    #[test]
    fn wrap_and_dyn_rewrites_preserve_protected_spans_and_utf8() {
        let source = r####"type Live = ::std::rc::Rc<dyn LiveTrait>;
const QUOTED: &str = "雪 ::std::rc::Rc<dyn Quoted>";
const ESCAPED: &str = "\"Rc<dyn Escaped>\"";
const RAW: &str = r##"文字 Rc<dyn Raw> and "quotes""##;
const BYTE: &[u8] = b"Rc<dyn Byte>";
const BYTE_RAW: &[u8] = br#"Rc<dyn ByteRaw>"#;
// Rc<dyn LineComment>
/* Rc<dyn Outer> /* ::std::rc::Rc<dyn Nested> */ */
"####;
        let expected = r####"type Live = ::std::sync::Arc<dyn LiveTrait + Send + Sync>;
const QUOTED: &str = "雪 ::std::rc::Rc<dyn Quoted>";
const ESCAPED: &str = "\"Rc<dyn Escaped>\"";
const RAW: &str = r##"文字 Rc<dyn Raw> and "quotes""##;
const BYTE: &[u8] = b"Rc<dyn Byte>";
const BYTE_RAW: &[u8] = br#"Rc<dyn ByteRaw>"#;
// Rc<dyn LineComment>
/* Rc<dyn Outer> /* ::std::rc::Rc<dyn Nested> */ */
"####;

        assert_eq!(
            transform_emitted_source(source, ThreadSafety::SendSync),
            expected
        );
    }

    #[test]
    fn marks_clone_static_bounds() {
        assert_eq!(
            insert_bound_markers("A: Clone + 'static", " + Send + Sync"),
            "A: Clone + Send + Sync + 'static"
        );
        assert_eq!(
            insert_bound_markers(
                "type Array<t: Clone + 'static>: Clone + PartialEq + 'static;",
                " + Send + Sync"
            ),
            "type Array<t: Clone + Send + Sync + 'static>: Clone + PartialEq + Send + Sync + 'static;"
        );
        assert_eq!(
            insert_bound_markers(
                "type app__I32: Clone + PartialEq + 'static + From<i32>;",
                " + Send + Sync"
            ),
            "type app__I32: Clone + PartialEq + Send + Sync + 'static + From<i32>;"
        );
        assert_eq!(
            insert_bound_markers("H: 'static", " + Send + Sync"),
            "H: Send + Sync + 'static"
        );
        assert_eq!(
            insert_bound_markers(
                "pub trait Poly: 'static {}\nfn new(f: impl Fn() + 'static) {}\nfn store<T: 'static>() {}",
                " + Send + Sync"
            ),
            "pub trait Poly: Send + Sync + 'static {}\nfn new(f: impl Fn() + Send + Sync + 'static) {}\nfn store<T: Send + Sync + 'static>() {}"
        );
    }

    #[test]
    fn marks_every_generated_static_bound_form() {
        let source = "pub(crate) fn store<T: 'static>() {}\n\
                      pub trait KioType: Clone + 'static {\n\
                          type Facade: Clone + 'static;\n\
                      }\n\
                      pub struct Product<A: Clone + 'static, B: Clone + 'static>;\n\
                      pub trait KioTypeConstructor1: Clone + 'static {}\n\
                      pub fn new(f: impl Fn() + 'static) {}\n\
                      pub fn body(f: impl Fn() + Clone + 'static) {}\n\
                      pub trait SiteForall: 'static {}\n\
                      pub struct Body<H: 'static, A: Clone + 'static>;\n\
                      pub trait Host: Clone + 'static {\n\
                          type Value: Clone + PartialEq + 'static + From<i32>;\n\
                          type BoxStorage: Clone + PartialEq + 'static;\n\
                          type Minimal: 'static + From<i32>;\n\
                          type MinimalStorage: 'static;\n\
                      }";
        let expected = "pub(crate) fn store<T: Send + Sync + 'static>() {}\n\
                        pub trait KioType: Clone + Send + Sync + 'static {\n\
                            type Facade: Clone + Send + Sync + 'static;\n\
                        }\n\
                        pub struct Product<A: Clone + Send + Sync + 'static, B: Clone + Send + Sync + 'static>;\n\
                        pub trait KioTypeConstructor1: Clone + Send + Sync + 'static {}\n\
                        pub fn new(f: impl Fn() + Send + Sync + 'static) {}\n\
                        pub fn body(f: impl Fn() + Clone + Send + Sync + 'static) {}\n\
                        pub trait SiteForall: Send + Sync + 'static {}\n\
                        pub struct Body<H: Send + Sync + 'static, A: Clone + Send + Sync + 'static>;\n\
                        pub trait Host: Clone + Send + Sync + 'static {\n\
                            type Value: Clone + PartialEq + Send + Sync + 'static + From<i32>;\n\
                            type BoxStorage: Clone + PartialEq + Send + Sync + 'static;\n\
                            type Minimal: Send + Sync + 'static + From<i32>;\n\
                            type MinimalStorage: Send + Sync + 'static;\n\
                        }";

        assert_eq!(insert_bound_markers(source, " + Send + Sync"), expected);
    }

    #[test]
    fn bound_markers_preserve_protected_spans_and_utf8() {
        let source = r####"pub trait Κ: 'static {
    const QUOTED: &str = "λ + 'static : 'static";
    const ESCAPED: &str = "\"雪: 'static + 'static";
    const RAW: &str = r##"文字 + 'static : 'static and "quotes""##;
    const BYTE_RAW: &[u8] = br#"bytes + 'static : 'static"#;
    // line λ + 'static : 'static
    /* outer 雪 + 'static /* nested : 'static */ still + 'static */
    fn capture(f: impl Fn() + 'static) {}
}
"####;
        let expected = r####"pub trait Κ: Send + Sync + 'static {
    const QUOTED: &str = "λ + 'static : 'static";
    const ESCAPED: &str = "\"雪: 'static + 'static";
    const RAW: &str = r##"文字 + 'static : 'static and "quotes""##;
    const BYTE_RAW: &[u8] = br#"bytes + 'static : 'static"#;
    // line λ + 'static : 'static
    /* outer 雪 + 'static /* nested : 'static */ still + 'static */
    fn capture(f: impl Fn() + Send + Sync + 'static) {}
}
"####;

        assert_eq!(insert_bound_markers(source, " + Send + Sync"), expected);
    }

    #[test]
    fn runtime_arc_is_valid_shape() {
        let r = runtime_arc(ThreadSafety::SendSync);
        // The erase primitives ride `Arc<dyn Any + Send + Sync>`.
        assert!(r.contains("use std::sync::Arc;"));
        assert!(r.contains("Arc<dyn Any + Send + Sync>"));
        // `as_any`'s payload bound gains the marker; `from_any`'s
        // `Clone + 'static` bound is marked by the shared pass.
        assert!(r.contains("as_any<T: Send + Sync + 'static>"));
        assert!(r.contains("from_any<T: Clone + Send + Sync + 'static>"));
        // No code-position `Rc` spellings survive. Protected comments may
        // still describe the single-threaded runtime, so verify that another
        // code-span rewrite is a no-op instead of scanning raw prose.
        assert_eq!(swap_rc_to_arc(&r), r);
    }
}
