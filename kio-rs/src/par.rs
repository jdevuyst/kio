//! Parallelism shim for the compiler's fan-out sites.
//!
//! Every fan-out in the compiler is a stateless map over an independent
//! collection (parse one file, optimize one module, emit one module,
//! …). The `maybe_par_iter!` family is the single seam that decides
//! whether that map runs across a rayon thread pool or sequentially:
//!
//! - with the `parallel` feature on (the default), each macro expands
//!   to a rayon `.par_iter()` / `.par_iter_mut()` / `.into_par_iter()`
//!   and the downstream `.map(…).collect()` runs in parallel;
//! - with `parallel` off, each expands to the matching sequential
//!   `.iter()` / `.iter_mut()` / `.into_iter()` and the same
//!   `.map(…).collect()` runs sequentially.
//!
//! Because the fan-outs are stateless maps, the two paths are
//! observationally identical — the sequential path is a pure
//! performance trade-off, not a behavior change.
//!
//! `parallel` off is what `wasm32-unknown-unknown` builds use:
//! `wasm32-unknown-unknown` has no thread pool, and rayon's iterators
//! additionally require the mapped closures to be `Send + Sync`, which
//! the AST payloads are not under that target — so the parallel arm
//! would not even type-check there. Turning the feature off also drops
//! rayon (and its threading internals) from the wasm dependency graph.
//!
//! Call sites read `maybe_par_iter!(collection).map(f).collect()`. The
//! trailing `.map(…)` is the caller's, not the macro's, so the
//! `ParallelIterator` / `IndexedParallelIterator` traits it resolves
//! against must be in scope *at the call site* — a glob `use` inside
//! the macro expansion does not reach a method chained after the
//! macro. Each fan-out module therefore carries its own
//! `#[cfg(feature = "parallel")] use rayon::prelude::*;`; with
//! `parallel` off the import is gone (along with the whole rayon
//! dependency) and the sequential arm's std `.iter()` needs nothing.

/// Iterate a slice for a stateless map fan-out — parallel under
/// `parallel`, sequential otherwise.
///
/// The argument is a slice or `Vec`. Under `parallel` the downstream
/// `.map(…)` closure must be `Send + Sync` (rayon's requirement); the
/// sequential arm accepts it unchanged.
#[cfg(feature = "parallel")]
#[macro_export]
macro_rules! maybe_par_iter {
    ($collection:expr) => {{ $collection.par_iter() }};
}

/// Sequential expansion of [`maybe_par_iter!`] (`parallel` off, e.g.
/// `wasm32-unknown-unknown`).
#[cfg(not(feature = "parallel"))]
#[macro_export]
macro_rules! maybe_par_iter {
    ($collection:expr) => {{ $collection.iter() }};
}

/// Mutable counterpart to [`maybe_par_iter!`] for the one fan-out that
/// maps over `&mut` elements (`op_fold`'s per-module fold).
#[cfg(feature = "parallel")]
#[macro_export]
macro_rules! maybe_par_iter_mut {
    ($collection:expr) => {{ $collection.par_iter_mut() }};
}

/// Sequential expansion of [`maybe_par_iter_mut!`].
#[cfg(not(feature = "parallel"))]
#[macro_export]
macro_rules! maybe_par_iter_mut {
    ($collection:expr) => {{ $collection.iter_mut() }};
}

/// Move-iterate a collection for a stateless map fan-out (the
/// `into_par_iter` sites) — parallel under `parallel`, sequential
/// otherwise.
#[cfg(feature = "parallel")]
#[macro_export]
macro_rules! maybe_into_par_iter {
    ($collection:expr) => {{ $collection.into_par_iter() }};
}

/// Sequential expansion of [`maybe_into_par_iter!`].
#[cfg(not(feature = "parallel"))]
#[macro_export]
macro_rules! maybe_into_par_iter {
    ($collection:expr) => {{ $collection.into_iter() }};
}

/// Append a line to a captured stdout buffer — the buffered analogue of
/// `println!`. The first argument is a
/// [`crate::cmd::package_fanout::CapturedOutput`]; the rest is a
/// `println!`-style format. Used by the multi-package fan-out so a
/// worker's output can be replayed in input order rather than racing to
/// the process stream. Self-contained: pulls in `std::fmt::Write` so the
/// call site needs no import.
#[macro_export]
macro_rules! cap_outln {
    ($cap:expr) => {{
        use ::std::fmt::Write as _;
        let _ = writeln!($cap.stdout);
    }};
    ($cap:expr, $($arg:tt)*) => {{
        use ::std::fmt::Write as _;
        let _ = writeln!($cap.stdout, $($arg)*);
    }};
}

/// Append a line to a captured stderr buffer — the buffered analogue of
/// `eprintln!`. See [`cap_outln!`].
#[macro_export]
macro_rules! cap_errln {
    ($cap:expr) => {{
        use ::std::fmt::Write as _;
        let _ = writeln!($cap.stderr);
    }};
    ($cap:expr, $($arg:tt)*) => {{
        use ::std::fmt::Write as _;
        let _ = writeln!($cap.stderr, $($arg)*);
    }};
}
