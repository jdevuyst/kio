//! Wadler-Leijen pretty-printer combinators.
//!
//! [`Doc`] is an intermediate representation built by [`crate::pretty`]
//! before rendering to a final `String`. Compared with the previous
//! `&mut String`-style emitter, the layered approach lets the renderer
//! make width-aware layout decisions in one place: each [`Doc::Group`]
//! tries to fit on the current line if it can, and otherwise breaks
//! every breakable position inside it.
//!
//! # Combinators
//!
//! - [`empty`] — the empty document.
//! - [`text`] — a literal string with no breaks inside.
//! - [`hardline`] — a newline that no [`Group`](Doc::Group) can flatten.
//! - [`softline`] — empty when fitted, newline when broken.
//! - [`line`] — single space when fitted, newline when broken.
//! - [`nest`] — increase the indent level for inner breaks.
//! - [`group`] — try to fit the inner document on one line; if that
//!   exceeds the width budget, break every soft/line position inside.
//! - [`concat`] — concatenate two documents.
//! - [`concat_all`] — concatenate a sequence of documents.
//!
//! # Rendering
//!
//! [`render`] walks the document with a target width (in columns) and
//! produces the final string. The "does this group fit?" check is the
//! standard one-line lookahead from Wadler's "A prettier printer" — for
//! each [`Group`](Doc::Group) the renderer asks whether the doc's
//! flattened width plus the current column fits in the budget; if it
//! does, breaks become the soft-alt text (space or empty); otherwise
//! breaks become `\n` + the current indent.
//!
//! `pretty.rs`'s top-level entry points (`pretty_module`,
//! `pretty_package_file`) build a [`Doc`] from the AST and call
//! [`render`] with the canonical 100-column budget. The same `render`
//! is used by every form, so the layout behaviour is uniform.

use std::borrow::Cow;
use std::rc::Rc;

/// The Doc combinator tree.
///
/// Recursive children sit behind [`Rc`] so cloning a `Doc` — which the
/// pretty-printer does constantly, e.g. splicing the same body into both
/// arms of a [`flat_alt`] — is an `Rc` bump rather than a deep copy. The
/// tree is therefore a shared DAG, not a tree: a body reused N times
/// across nested layout alternatives contributes O(1) nodes per reuse,
/// not O(2^depth). Rendering still descends each [`FlatAlt`] into exactly
/// one arm, so a shared subtree is laid out at most once per chosen
/// layout — the DAG never re-expands.
#[derive(Debug, Clone)]
pub enum Doc {
    /// The empty document. Renders as nothing.
    Empty,
    /// A literal string. Must not contain newlines — use [`Doc::Line`]
    /// or [`hardline`] for those.
    Text(Cow<'static, str>),
    /// A breakable position. The behaviour depends on whether the
    /// enclosing [`Doc::Group`] fitted on one line:
    /// - If fitted, the `flat` text is emitted (typically `""` for
    ///   [`softline`] or `" "` for [`line`]).
    /// - If broken, a `\n` is emitted followed by the current indent.
    ///
    /// `hard` lines pretend the enclosing group did not fit even if
    /// it would — they always break. Used for between-item separators
    /// and inside multi-line layouts that don't depend on width.
    Line { flat: Cow<'static, str>, hard: bool },
    /// Layout alternative: render the first sub-doc when the
    /// enclosing [`Doc::Group`] is flattened, and the second when it
    /// breaks. The fits-check uses the flat (first) sub-doc — so
    /// this is the right primitive for "structurally different
    /// flat vs broken layouts," like the leading-comma A1 layout
    /// that's `(a, b, c)` flat but `(\n  , a\n  , b\n  )` broken.
    FlatAlt(Rc<Doc>, Rc<Doc>),
    /// Increase the indent level by `n` for inner [`Doc::Line`]
    /// emissions. Doesn't emit anything itself.
    Nest(usize, Rc<Doc>),
    /// Set the indent level to the current column for inner
    /// [`Doc::Line`] emissions. Useful for "lines align with where
    /// this doc started" — e.g., string-literal reflow where
    /// continuation chunks line up with the first `"`.
    Align(Rc<Doc>),
    /// Try to fit the inner doc on one line. If the doc's flattened
    /// width plus the current column exceeds the renderer's width
    /// budget, break instead.
    Group(Rc<Doc>),
    /// Several docs side by side. Behind an [`Rc`] so cloning the
    /// concatenation shares the slice instead of copying every part.
    Concat(Rc<Vec<Doc>>),
}

// ---- constructors --------------------------------------------------------

/// The empty document. `concat_all` skips empties; this is mostly
/// useful as a leaf inside `if`/`else` branches that build optional
/// pieces.
pub fn empty() -> Doc {
    Doc::Empty
}

/// A literal piece of text. Must not contain newlines.
pub fn text(s: impl Into<Cow<'static, str>>) -> Doc {
    Doc::Text(s.into())
}

/// An unconditional line break. The enclosing [`group`] cannot flatten
/// this — emit `\n` + indent regardless of whether the group fits.
pub fn hardline() -> Doc {
    Doc::Line {
        flat: Cow::Borrowed(""),
        hard: true,
    }
}

/// A breakable line that's empty when its [`group`] fits and `\n` +
/// indent when it breaks. Use this for "I'd like an optional break
/// here if the group is too wide."
pub fn softline() -> Doc {
    Doc::Line {
        flat: Cow::Borrowed(""),
        hard: false,
    }
}

/// A breakable line that's a single space when its [`group`] fits and
/// `\n` + indent when it breaks.
pub fn line() -> Doc {
    Doc::Line {
        flat: Cow::Borrowed(" "),
        hard: false,
    }
}

/// Increase the indent level by `n` for breaks inside `inner`.
pub fn nest(n: usize, inner: Doc) -> Doc {
    Doc::Nest(n, Rc::new(inner))
}

/// Set the indent level to the current column for breaks inside
/// `inner`. Use this for "continuation lines align here" — e.g.,
/// the chunks of a reflowed string literal line up with the first
/// `"` rather than with the surrounding indent.
pub fn align(inner: Doc) -> Doc {
    Doc::Align(Rc::new(inner))
}

/// Try to fit `inner` on one line. If it doesn't fit, break every
/// soft/line position inside.
pub fn group(inner: Doc) -> Doc {
    Doc::Group(Rc::new(inner))
}

/// Layout alternative: pick `flat` when the enclosing group fits and
/// `broken` when it doesn't. Use this when the two layouts are
/// structurally different (e.g., the leading-comma A1 list shape
/// where flat `(a, b, c)` and broken `(\n  , a\n  , b\n  )` aren't
/// related by line-substitution).
pub fn flat_alt(flat: Doc, broken: Doc) -> Doc {
    Doc::FlatAlt(Rc::new(flat), Rc::new(broken))
}

/// Concatenate two docs.
///
/// Flattens nested `Concat`s into one slice. The `Rc<Vec<Doc>>` payloads
/// are unwrapped with [`Rc::try_unwrap`] when uniquely owned (the common
/// case during bottom-up Doc construction) so the merge stays an in-place
/// `Vec` mutation; a shared `Concat` falls back to cloning its parts.
pub fn concat(a: Doc, b: Doc) -> Doc {
    fn into_vec(parts: Rc<Vec<Doc>>) -> Vec<Doc> {
        Rc::try_unwrap(parts).unwrap_or_else(|shared| (*shared).clone())
    }
    match (a, b) {
        (Doc::Empty, b) => b,
        (a, Doc::Empty) => a,
        (Doc::Concat(left), Doc::Concat(right)) => {
            let mut left = into_vec(left);
            let mut right = into_vec(right);
            left.append(&mut right);
            Doc::Concat(Rc::new(left))
        }
        (Doc::Concat(left), b) => {
            let mut left = into_vec(left);
            left.push(b);
            Doc::Concat(Rc::new(left))
        }
        (a, Doc::Concat(right)) => {
            let mut right = into_vec(right);
            right.insert(0, a);
            Doc::Concat(Rc::new(right))
        }
        (a, b) => Doc::Concat(Rc::new(vec![a, b])),
    }
}

/// Concatenate a sequence of docs.
pub fn concat_all<I: IntoIterator<Item = Doc>>(parts: I) -> Doc {
    let mut acc = Doc::Empty;
    for d in parts {
        acc = concat(acc, d);
    }
    acc
}

/// Join a sequence of docs with `sep` between each pair.
pub fn join<I: IntoIterator<Item = Doc>>(sep: Doc, parts: I) -> Doc {
    let mut acc = Doc::Empty;
    let mut first = true;
    for d in parts {
        if first {
            acc = d;
            first = false;
        } else {
            acc = concat(acc, concat(sep.clone(), d));
        }
    }
    acc
}

/// Build a comma-separated list with the canonical A1 layout —
/// flat `(a, b, c)` if the whole group fits in the renderer's
/// width budget, else broken to leading-comma multi-line:
///
/// ```text
/// (
///   , a
///   , b
///   , c
///   )
/// ```
///
/// `open` and `close` are the opener/closer texts (typically `"("`
/// and `")"`, but can be `"<"` / `">"` or `"{"` / `"}"`). `items`
/// are the list elements; the helper handles separators and the
/// leading-comma layout switch. In the broken layout an item that
/// spans multiple lines lays out relative to its own content column
/// (the column after `, `), so e.g. a lambda item's block body sits
/// at +2 from the lambda header and its `}` returns to the item's
/// content column.
///
/// **Single-element lists.** If `items.len() == 1`, the helper
/// emits the item single-line regardless of width — A1's
/// "single-line at 0–1 items unconditionally" rule overrides.
/// **Empty lists** emit `open close` with no separators.
pub fn comma_list<I: IntoIterator<Item = Doc>>(open: Doc, items: I, close: Doc) -> Doc {
    comma_list_with_padding(open, items, close, false)
}

/// Symbol-run delimiters need spaces in the flat layout to remain distinct
/// lexical tokens. Broken layout already supplies those boundaries as newlines.
pub fn comma_list_with_padding<I: IntoIterator<Item = Doc>>(
    open: Doc,
    items: I,
    close: Doc,
    padded: bool,
) -> Doc {
    let items: Vec<Doc> = items.into_iter().collect();
    let padding = if padded { text(" ") } else { empty() };
    let flat = concat_all([
        open.clone(),
        padding.clone(),
        join(text(", "), items.clone()),
        if items.is_empty() { empty() } else { padding },
        close.clone(),
    ]);
    if items.len() <= 1 {
        // A1: 0 or 1 items always single-line, no width check.
        return flat;
    }

    // Broken layout: `open` + nest(2, hardline + `, item` + hardline
    // + `, item` + ... + hardline + `close`). The trailing hardline
    // and the closer sit inside the same nest scope so the closer
    // ends up at the items' +2 indent column, per A1. Each item gets
    // a further nest(2) mirroring the `", "` prefix: the prefix
    // advances the column without moving the indent level, so without
    // the nest a multi-line item's continuation lines would anchor at
    // the comma column, two columns left of the item's own start.
    let mut broken_inner = Doc::Empty;
    for item in &items {
        broken_inner = concat(
            broken_inner,
            concat(hardline(), concat(text(", "), nest(2, item.clone()))),
        );
    }
    broken_inner = concat(broken_inner, hardline());
    let broken = concat(open, nest(2, concat(broken_inner, close)));

    group(flat_alt(flat, broken))
}

// ---- rendering -----------------------------------------------------------

/// Render `doc` to a string, targeting `width` columns. The width is
/// the soft cap for groups: a group that fits flattened within the
/// remaining budget is emitted on one line; otherwise its breaks fire.
pub fn render(doc: &Doc, width: usize) -> String {
    let mut out = String::new();
    let mut cursor = Cursor {
        col: 0,
        out: &mut out,
    };
    render_doc(doc, &Mode::Break, 0, width, &mut cursor);
    // A `hardline()` inside a `nest(n)` emits `\n` followed by `n`
    // indent spaces eagerly — when the next thing on that line is
    // another break (a blank line between nested sections), those
    // spaces are left dangling as trailing whitespace. Strip
    // trailing spaces from every line; canonical Kio source carries
    // none, so this is purely corrective.
    strip_trailing_spaces(&out)
}

/// Remove trailing ASCII spaces from every line of `s`, preserving the
/// line structure (including a final trailing newline).
fn strip_trailing_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end_matches(' '));
    }
    out
}

/// Render mode for a sub-document.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// `Doc::Line` emits its `flat` text (no actual newline). `hard`
    /// lines break out of `Flat` mode by emitting `\n` anyway.
    Flat,
    /// `Doc::Line` emits `\n` + indent.
    Break,
}

struct Cursor<'a> {
    col: usize,
    out: &'a mut String,
}

impl<'a> Cursor<'a> {
    fn write_str(&mut self, s: &str) {
        // No `\n` is expected here — `Text` documents are line-free.
        // If a caller did pass a newline, the column count goes
        // monotonically wrong for that line; we still emit it
        // verbatim.
        self.out.push_str(s);
        self.col += s.chars().count();
    }
    fn newline_indent(&mut self, indent: usize) {
        self.out.push('\n');
        for _ in 0..indent {
            self.out.push(' ');
        }
        self.col = indent;
    }
}

/// Walk `doc` emitting characters into `cursor`. `mode` determines how
/// `Doc::Line` is interpreted; `indent` is the current indent column;
/// `width` is the renderer's budget. Indent is propagated through
/// nest levels by [`render_doc`]'s recursive calls.
fn render_doc(doc: &Doc, mode: &Mode, indent: usize, width: usize, cursor: &mut Cursor<'_>) {
    match doc {
        Doc::Empty => {}
        Doc::Text(s) => cursor.write_str(s),
        Doc::Line { flat, hard } => match mode {
            Mode::Flat if !hard => cursor.write_str(flat),
            _ => cursor.newline_indent(indent),
        },
        Doc::FlatAlt(flat, broken) => match mode {
            Mode::Flat => render_doc(flat, mode, indent, width, cursor),
            Mode::Break => render_doc(broken, mode, indent, width, cursor),
        },
        Doc::Nest(n, inner) => render_doc(inner, mode, indent + n, width, cursor),
        Doc::Align(inner) => render_doc(inner, mode, cursor.col, width, cursor),
        Doc::Group(inner) => {
            // Try flat mode first if this group has any soft/non-hard
            // breaks that could even flatten. Then check whether the
            // flat width fits in the remaining budget.
            if fits(inner, width.saturating_sub(cursor.col)) {
                render_doc(inner, &Mode::Flat, indent, width, cursor);
            } else {
                render_doc(inner, &Mode::Break, indent, width, cursor);
            }
        }
        Doc::Concat(parts) => {
            for p in parts.iter() {
                render_doc(p, mode, indent, width, cursor);
            }
        }
    }
}

/// Could `doc`'s flattened form fit in `remaining` columns? The walk
/// is conservative — it sums up text widths assuming every
/// non-`hard` `Doc::Line` becomes its `flat` form, and bails as soon
/// as the running total exceeds `remaining`. A `hard` line forces a
/// negative answer (the group can't flatten through a hard break).
fn fits(doc: &Doc, remaining: usize) -> bool {
    let mut budget = remaining as isize;
    fits_inner(doc, &mut budget)
}

fn fits_inner(doc: &Doc, budget: &mut isize) -> bool {
    if *budget < 0 {
        return false;
    }
    match doc {
        Doc::Empty => true,
        Doc::Text(s) => {
            *budget -= s.chars().count() as isize;
            *budget >= 0
        }
        Doc::Line { flat, hard } => {
            if *hard {
                false
            } else {
                *budget -= flat.chars().count() as isize;
                *budget >= 0
            }
        }
        // The fits-check measures the flat (first) sub-doc; the
        // broken alternative is consulted only at render time when
        // the enclosing group has chosen to break.
        Doc::FlatAlt(flat, _) => fits_inner(flat, budget),
        Doc::Nest(_, inner) => fits_inner(inner, budget),
        Doc::Align(inner) => fits_inner(inner, budget),
        Doc::Group(inner) => fits_inner(inner, budget),
        Doc::Concat(parts) => parts.iter().all(|p| fits_inner(p, budget)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_renders_nothing() {
        assert_eq!(render(&empty(), 80), "");
    }

    #[test]
    fn text_renders_verbatim() {
        assert_eq!(render(&text("hello"), 80), "hello");
    }

    #[test]
    fn concat_renders_in_order() {
        assert_eq!(render(&concat(text("a"), text("b")), 80), "ab");
    }

    #[test]
    fn join_separates_with_sep() {
        let d = join(text(", "), vec![text("a"), text("b"), text("c")]);
        assert_eq!(render(&d, 80), "a, b, c");
    }

    #[test]
    fn hardline_always_breaks() {
        let d = concat(text("a"), concat(hardline(), text("b")));
        assert_eq!(render(&d, 80), "a\nb");
    }

    #[test]
    fn group_fits_flattens_softlines() {
        // softline -> "" when fitted
        let d = group(concat(text("("), concat(softline(), text(")"))));
        assert_eq!(render(&d, 80), "()");
    }

    #[test]
    fn group_fits_flattens_lines_to_spaces() {
        let d = group(concat(text("a"), concat(line(), text("b"))));
        assert_eq!(render(&d, 80), "a b");
    }

    #[test]
    fn group_overflow_breaks_softlines() {
        // 100-char text; 10-col budget; softline becomes \n
        let big = "x".repeat(20);
        let d = group(concat(
            text("("),
            concat(softline(), concat(text(big.clone()), softline())),
        ));
        // Output begins with '(' then breaks; closing ')' isn't there
        // because we omitted it in this test
        let rendered = render(&d, 10);
        assert!(rendered.contains('\n'), "broke: {rendered:?}");
    }

    #[test]
    fn nest_indents_breaks() {
        // Standard Wadler "(\n  a\n  b\n)" shape: items at +2 indent,
        // closing paren back at the outer indent. The trailing `line`
        // sits outside `nest` so it breaks at column 0.
        let d = group(concat(
            text("("),
            concat(
                nest(
                    2,
                    concat(line(), concat(text("a"), concat(line(), text("b")))),
                ),
                concat(line(), text(")")),
            ),
        ));
        let rendered = render(&d, 3);
        assert_eq!(rendered, "(\n  a\n  b\n)");
    }

    #[test]
    fn nested_group_fits_independently() {
        // Outer group too wide to fit; inner group fits.
        let inner = group(concat(text("a"), concat(line(), text("b"))));
        let outer = group(concat(
            text("AAAAAAA"),
            concat(line(), concat(inner, concat(line(), text("BBBBBBB")))),
        ));
        // width 10: outer cannot fit (~25 chars flat), so its lines
        // break; inner ("a b" — 3 chars) fits and stays flat.
        let rendered = render(&outer, 10);
        // inner stays flat: "a b" appears as a contiguous segment
        assert!(rendered.contains("a b"), "inner flat: {rendered:?}");
        // outer's lines broke: there's at least one '\n'
        assert!(rendered.contains('\n'), "outer broke: {rendered:?}");
    }

    #[test]
    fn hardline_inside_group_forces_break() {
        // Even though "ab" trivially fits, a hardline forces break mode.
        let d = group(concat(text("a"), concat(hardline(), text("b"))));
        assert_eq!(render(&d, 80), "a\nb");
    }

    #[test]
    fn flat_alt_picks_flat_when_group_fits() {
        let d = group(flat_alt(text("flat"), text("broken")));
        assert_eq!(render(&d, 80), "flat");
    }

    #[test]
    fn flat_alt_picks_broken_when_group_doesnt_fit() {
        // The flat width is 30 — overflows a 10-col budget.
        let d = group(flat_alt(text("XXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"), text("Y")));
        assert_eq!(render(&d, 10), "Y");
    }

    #[test]
    fn comma_list_zero_items_is_open_close() {
        let d = comma_list::<Vec<Doc>>(text("("), vec![], text(")"));
        assert_eq!(render(&d, 80), "()");
    }

    #[test]
    fn comma_list_one_item_stays_single_line() {
        let d = comma_list(text("("), vec![text("a")], text(")"));
        assert_eq!(render(&d, 80), "(a)");
    }

    #[test]
    fn comma_list_fits_renders_flat() {
        let d = comma_list(text("("), vec![text("a"), text("b"), text("c")], text(")"));
        assert_eq!(render(&d, 80), "(a, b, c)");
    }

    #[test]
    fn comma_list_overflow_breaks_to_a1() {
        // Three 30-char items, 50-col budget — broken form fires.
        let d = comma_list(
            text("("),
            vec![text("xxxx"), text("yyyy"), text("zzzz")],
            text(")"),
        );
        assert_eq!(render(&d, 10), "(\n  , xxxx\n  , yyyy\n  , zzzz\n  )");
    }

    #[test]
    fn align_sets_indent_to_current_column() {
        // Emit "prefix " (7 chars), then align inner content so its
        // hardlines indent to column 7.
        let inner = concat(
            text("a"),
            concat(hardline(), concat(text("b"), concat(hardline(), text("c")))),
        );
        let d = concat(text("prefix "), align(inner));
        assert_eq!(render(&d, 80), "prefix a\n       b\n       c");
    }

    // Regression: building a `Doc` must not be exponential in nesting
    // depth. The pretty-printer splices the same body into multiple
    // places — most notably both arms of a `flat_alt` (every block body:
    // the flat ` { body }` and the broken multi-line form). When the
    // recursive children were `Box<Doc>`, `Doc: Clone` was a deep copy,
    // so each such reuse DOUBLED the stored node count and an N-deep
    // nesting was O(2^N) nodes — a real `.kio` file with a deeply nested
    // body (a host-`loop` tree walk) exhausted memory at `kio build` when
    // it rendered Kio' source for the enriched-IR cache key.
    //
    // This builds exactly that pattern — `flat_alt(body, body)` nested
    // `DEPTH` levels, the body reused in both arms at every level — at a
    // depth whose deep-copy node count (`2^DEPTH`) is astronomically
    // beyond any machine's memory. With recursive children behind `Rc`
    // the clone is a pointer bump and the structure is a linear DAG, so
    // both the build and the render finish instantly. The test passing at
    // all is the assertion: pre-fix it could not allocate the tree.
    #[test]
    fn deeply_shared_flat_alt_is_not_exponential() {
        // 2^200 is ~1.6e60 — astronomically beyond any machine's memory,
        // so a deep-copying `Clone` could never build the tree; yet 200
        // levels is a shallow recursion for `render` / `fits` and the
        // `Drop` of the DAG, so the post-`Rc` structure runs instantly on
        // the smallest box.
        const DEPTH: usize = 200;
        let mut d = text("leaf");
        for _ in 0..DEPTH {
            // Reuse `d` in both arms — the `flat_alt` clone the
            // block-body layout performs. With Rc each `d.clone()` is a
            // bump; with Box it would deep-copy, doubling node count.
            d = group(flat_alt(d.clone(), nest(2, d)));
        }
        // Flat mode is chosen (everything fits), so render descends the
        // first arm at each level — DEPTH steps, linear. A pre-fix build
        // never reached this line: constructing `d` already needed 2^DEPTH
        // nodes.
        assert_eq!(render(&d, 80), "leaf");
    }
}
