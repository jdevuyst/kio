//! Shared host-body rendering vocabulary.
//!
//! The selected runner protocol supplies every host function's
//! operational meaning as a structured `HostFnBodyKind`. Typed-native
//! adapters may translate that structure into [`CanonicalKind`] as a
//! convenient common rendering vocabulary, then render the
//! backend-specific body.
//!
//! This module never derives semantics from emitted names, signatures,
//! or generated source. A host function's module and leaf choose only
//! its emitted member name; its protocol body determines behavior.
//!
//! ## Why a kind rather than a string body
//!
//! Two reasons.
//!
//! 1. **Host-language coupling.** A "body" is Rust source for the
//!    Rust runner and a JS expression for the JS runner. Sharing the
//!    string would mean parameterising the string over half a dozen
//!    host-language fragments — at which point we've reinvented a
//!    bad code-generator. A kind tag lets each runner emit native
//!    source directly.
//!
//! 2. **Validation.** A kind is enumerable: the per-backend runner's
//!    `match` is exhaustive, and adding a new canonical shape is a
//!    coordinated protocol + adapter change. A string-keyed table can
//!    drift silently.

/// The kinds of host fn the runners commit to providing a canonical
/// default implementation for.
///
/// Each typed-native adapter maps a structured protocol body to one of
/// these variants and renders it in the host language. The variant data
/// carries the rendering's required inputs (the kind suffix `i32` /
/// `i64`, the op token `add` / `sub`, …) and nothing else.
#[cfg(feature = "typed-native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalKind {
    /// `host fn print(s: Str) -> .;` — write to stdout (no
    /// implicit newline, the module code controls them).
    Print,
    /// `host fn eprint(s: Str) -> .;` — write to stderr.
    Eprint,
    /// `host fn exit(n: Int) -> !;` — process exit. Per-backend
    /// runners clamp into `0..=125` (see `clamp_exit`).
    Exit,
    /// `host fn read_ascii_line() -> Str | .;` — read the next ASCII
    /// line from the runner process stdin. EOF returns unit; non-ASCII
    /// input is a runner error.
    ReadAsciiLine,
    /// `host fn string_len(s: Str) -> Int;` — string byte length.
    StringLen,
    /// `host fn string_slice(s: Str, start: Int, end: Int) -> Str;` —
    /// half-open byte slice; invalid ranges fail loudly.
    StringSlice,
    /// `host fn string_code_at(s: Str, index: Int) -> Int | .;` —
    /// byte value at an index, or unit when out of bounds.
    StringCodeAt,
    /// `host fn string_concat(a: Str, b: Str) -> Str;` — string `+`
    /// in JS, `format!` in Rust.
    StringConcat,
    /// `host fn string_eq(a: Str, b: Str) -> Bool;` — character-
    /// by-character string equality (`===` in JS, `==` in Rust).
    /// Surface kio has no `==` operator, and equality on the
    /// `String` host type can't be expressed in terms of the
    /// existing primitives (`string_concat` produces strings but
    /// gives no way to inspect them) — so goldens that need it
    /// declare `host fn string_eq(p0: String, p1: String) -> Bool;` and
    /// the runner supplies the body.
    StringEq,
    /// `host fn loop[S][R](step: S -> (S | R), state: S) -> R;`
    /// — the general-recursion primitive. Per the spec, the step
    /// function returns a tagged sum (`[0, new_state]` continues,
    /// `[1, result]` exits). The Rust runner uses
    /// `extra_loop_sum_path()` (set on the per-backend side from the
    /// where-clause) to find the shape enum.
    Loop,
    /// `host fn <kind>_to_string(v: <Kind>) -> Str;` — stringify a
    /// numeric value, keyed by `kind` (`String(v)` /
    /// `.to_string()`, same for a JS `Number` or `BigInt`).
    NumericToString { kind: String },
    /// `host fn bool_to_string(v: Bool) -> Str;` — stringify a
    /// bool.
    BoolToString,
    /// `host fn print_i32(v: I32) -> .;` — write an i32 decimal to
    /// stdout.
    PrintI32,
    /// `host fn string_to_int(s: Str) -> Int | .;` — parse signed
    /// base-10 i32 text. Invalid or out-of-range input returns unit.
    StringToInt,
    /// `host fn <op>_<kind>(a: <Kind>, b: <Kind>) -> <Kind>;` — the
    /// fixed-width integer arithmetic family, keyed by `op`
    /// (`add` / `sub` / `mul` / `div` / `mod`) and `kind` (any
    /// `i8`…`i128` / `u8`…`u128`); wraps at the kind's width.
    Arith { op: String, kind: String },
    /// `host fn <op>_<kind>(a: <Kind>, b: <Kind>) -> <Kind>;` for a
    /// float kind (`f32` / `f64`) — `add` / `sub` / `mul` / `div`. The
    /// IEEE-754 ops apply directly (no width-mod wrap), so floats are a
    /// separate kind from the integer `Arith`.
    FloatArith { op: String, kind: String },
    /// `host fn <cmp>_<kind>(a: <Kind>, b: <Kind>) -> Bool;` — the
    /// fixed-width integer comparison family, keyed by `cmp`
    /// (`eq` / `lt` / `leq` / `le` / `gt` / `geq` / `ge`) and `kind`.
    Cmp { cmp: String, kind: String },
    /// `host fn make_scalar(text: Str, representation: Str) -> Scalar;` — the
    /// opaque-scalar constructor backing `dyn_load_prime`'s `host type Scalar`.
    /// Parses the scalar from a literal's text and the fixture representation
    /// key selected from its exact host-type descriptor.
    MakeScalar,
    /// `host fn scalar_of_<kind>(v: <Kind>) -> Scalar;` — lift a typed host
    /// value into the opaque `Scalar` box (`i32` / `str` / `bool` / `f64`).
    ScalarOf { kind: String },
    /// `host fn scalar_as_<kind>(s: Scalar) -> . | <Kind>;` — project a
    /// typed host value back out of the opaque `Scalar`, or `()` on a shape
    /// mismatch.
    ScalarAs { kind: String },
    /// `host fn scalar_is_true(s: Scalar) -> Bool;` — test an opaque bool
    /// scalar, for `dyn_load_prime`'s conditional decision.
    ScalarIsTrue,
    /// One of the polymorphic `array_*` primitives backing the
    /// canonical `host type Array[T];` surface. The variant tag
    /// names the operation; per-backend runners pick a backing
    /// representation (plain JS array, Rust `Vec<T>`, …) and render
    /// the matching body. The full surface mirrors the JS runner's
    /// ten-item polymorphic record (see `kio-test-runner-js` § Arrays).
    Array(ArrayOp),
    /// A closed protocol-specific [`HostFnBodyKind`](crate::protocol::HostFnBodyKind)
    /// variant that does not use a shared canonical renderer. The adapter
    /// handles that variant directly; exhaustive matching makes a newly added
    /// body a compile error until every adapter implements it.
    Custom,
}

/// One of the ten `array_*` polymorphic primitives backing the
/// canonical `host type Array[T];` surface. The signatures match
/// the JS runner's record (see `kio-test-runner-js` § Arrays).
#[cfg(feature = "typed-native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayOp {
    /// `host fn array_make_empty[T]() -> Array(T);`
    MakeEmpty,
    /// `host fn array_make_filled[T](n: I32, fill: T) -> Array(T);`
    MakeFilled,
    /// `host fn array_len[T](a: Array(T)) -> I32;`
    Len,
    /// `host fn array_get[T](a: Array(T), i: I32) -> T;`
    Get,
    /// `host fn array_set[T](a: Array(T), i: I32, v: T) -> .;`
    Set,
    /// `host fn array_push[T](a: Array(T), v: T) -> .;`
    Push,
    /// `host fn array_pop_back[T](a: Array(T)) -> T | .;`
    PopBack,
    /// `host fn array_swap[T](a: Array(T), i: I32, j: I32) -> .;`
    Swap,
    /// `host fn array_clear[T](a: Array(T)) -> .;`
    Clear,
    /// `host fn array_clone[T](a: Array(T)) -> Array(T);`
    Clone,
}
