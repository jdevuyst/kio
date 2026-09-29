#![forbid(dead_code)]

//! Library entry point for the `kio-rs` implementation.
//!
//! Two binary front-ends share the bulk of the compiler:
//!
//! - `kio` — the regular Kio compiler (compiles full Kio). Built
//!   under `--features surface`; runs `op_fold`, `desugar`, and `label_elab`
//!   for surface lowering and `typecheck_full` for the elaboration-aware
//!   typer. `pass::substitute` materializes its records, then `prime::typer`
//!   validates and canonicalizes the resulting Prime artifact.
//! - `kio-prime` — only accepts Kio'; rejects every surface-only
//!   form during the Surface → Prime walk with a parse error.
//!   Built under `--features prime`; pulls in `prime::lower` and
//!   `prime::pipeline`, then uses the same standalone `prime::typer`
//!   validator (which routes through the shared `typecheck_core`
//!   machinery via the `Typer<Prime>` dispatch trait). Useful as a
//!   Kio'-shaped reference and to gate test goldens that claim
//!   Kio'-ness.
//!
//! Both call [`run`] with the same argv slice, differing only in the
//! `prime_only` flag. See `src/main.rs` and `src/bin/kio-prime.rs` for
//! the thin entry shells.
//!
//! ## Module layout
//!
//! Shared (always compiled):
//!
//! - [`lexer`], [`parser`] — surface-syntax pipeline producing a
//!   `Module<Surface>`. Each [`lexer::Token`] carries leading-only
//!   trivia (line comments + newlines) so `kio fmt` can preserve
//!   docstring-style comments through a round-trip.
//! - [`resolve`] — name resolution. `Package::build`,
//!   `resolve_imports`, `check_no_value_cycles`, and the in-body
//!   `Resolver` are all phase-polymorphic (the resolver via the
//!   `ResolvePhase` bound), so the kio-prime pipeline runs the
//!   resolver against `Module<Prime>` directly while the kio binary
//!   uses `Module<Lowered>`.
//! - [`pass::alpha_normalize`] — gives lexical type binders stable identities
//!   before either checking path runs.
//! - [`typecheck_core`] — phase-polymorphic typer machinery:
//!   `ModuleEnv<P>`, `TypeCtx<P>`, `Synth<P>`, the synth/check/apply
//!   helpers (`synth_path` / `synth_let` / `synth_fn` / `synth_call`
//!   / `apply_*` / `check_fn_def` / the scoped package checker / etc.),
//!   plus pure type helpers (`subst_type`, `display_type`,
//!   `unfold_top`, `type_equiv`, `unify_pattern`,
//!   `check_newtype_payload`, …). The `Typer<P>` dispatch trait
//!   threads the recursive `synth_expr` / `check_value_against`
//!   calls through a per-phase wiring (`LoweredTyper` /
//!   `PrimeTyper`) so the same helpers serve both binaries.
//! - [`pipeline`] — the `Pipeline` trait the check / build / fmt
//!   drivers dispatch through.
//! - [`pass::structural_recovery`] — post-validation `Module<Prime>` →
//!   `Module<Enriched>` pass: collapses right-leaning intrinsic
//!   chains into the enriched IR's n-ary structural nodes for optimization.
//! - [`pass::recover_to_low`] — post-`structural_recovery`
//!   `Module<Enriched>` → `Module<Routed>` pass: folds the
//!   package's resolution context (host-fn / module-fn /
//!   use-import / qualified-import / newtype tables) into
//!   pre-classified `Expr::Low*` call nodes. Host backends consume
//!   `Module<Routed>` directly.
//! - [`pass::capabilities`] — post-`recover_to_low` per-module suite of
//!   passes that write capability annotations onto the Routed AST:
//!   outer-scope captures on each `Expr::FnExpr` and
//!   [`crate::ast::Lifetime`] on each `Type::Function`. The
//!   `captured_from` captures are an internal cross-pass input: the
//!   escape pass writes them, the lifetime pass reads them to derive
//!   `Lifetime`. The Rust backend reads only `Lifetime` (`Rc<dyn Fn>`
//!   vs `impl Fn`); JS ignores it. The framework is shaped to admit
//!   further per-position annotations later without restructuring.
//! - [`backends::js::emit`] — JS backend, consuming `Module<Routed>` (the
//!   post-resolution-lowering IR). [`backends::kio_prime`] — Kio'-emit
//!   backend, consuming `Module<Prime>` directly (it re-emits Kio'
//!   source, which the enriched IR is not).
//! - [`pretty`] / [`cmd::fmt`] / [`doc`] — back end of `kio fmt`. Emits
//!   the canonical style specified in `specs/style.md` (A1
//!   leading-comma layout, four-block use ordering, lowercase-hex
//!   / lowercase-`e` literal canonicalisation, top-level trivia
//!   preservation).
//!
//! Full-Kio-only (compiled under `--features surface`):
//!
//! - [`pass::op_fold`] — folds surface operator chains before desugaring.
//! - [`pass::desugar`] — Surface → Desugared (strips `Expr::Tuple`,
//!   `Expr::FnPlaceholder`).
//! - [`pass::label_elab`] — Desugared → Lowered (strips `Expr::LabelValue`,
//!   `Type::LabelSugar`, `Item::Labels`).
//! - [`pass::typecheck_full`] — elaboration-aware Kio typer; records
//!   surface-form rewrites and inferred completions while checking Lowered,
//!   then `substitute_package` bakes them into the AST at the Lowered → Prime
//!   transition and `prime::typer` validates and canonicalizes the result.
//! - [`pass::substitute`] — Lowered → Prime substitution that consumes
//!   `Elaborations`.
//! - [`pass::full`] — `Pipeline for FullPipeline`.
//!
//! Shared Prime completion and validation (compiled when `surface`, `prime`,
//! or `cli` is enabled):
//!
//! - [`prime::typer`] — standalone Kio'-only typer entry point
//!   plus the `PrimeTyper` impl of `Typer<Prime>`; it bakes the narrow
//!   call/lambda completion table before returning checked Prime.
//! - [`prime::canonical`] — capture-avoiding statement-spine normalization
//!   applied to checked Prime.
//!
//! Kio-prime front end (compiled under `--features prime`):
//!
//! - [`prime::lower`] — Surface → Prime walk; rejects every
//!   surface-only variant with a parse error. The Kio'-only mirror
//!   of `desugar` + `label_elab`.
//! - [`prime::pipeline`] — `Pipeline for PrimePipeline`.

#![forbid(unsafe_code)]

#[macro_use]
mod par;
mod timing;

pub mod ast;
#[cfg(any(test, feature = "cli"))]
pub(crate) mod build_target;
#[cfg(all(feature = "surface", feature = "cli"))]
pub(crate) mod builtin_docs;
pub mod comptime;
// `package_collection` holds core type definitions and the workspace-walk
// cluster used by the CLI drivers while keeping pure types available
// with `cli` off.
pub mod package_collection;
// `doc` / `doc_entry` are the pure doc-comment AST (`DocComment`,
// `DocBlock`, …) the pretty-printer and the Kiodoc renderer both
// consume; they carry no fs / process, so they stay core.
pub mod diagnostic;
pub mod doc;
pub mod doc_entry;
pub mod error;
pub mod exit_code;
pub mod file_kind;
// `git_dep` resolves a `source { git; ref }` dependency: it drives `git`
// to clone into the per-user cache and check out the locked / resolved
// commit, then writes the per-dependency `<local>.lock.kio` pin. Runs at
// the same command-level materialization step as `package_collection`,
// so it is `cli`-gated alongside it.
#[cfg(feature = "cli")]
pub mod git_dep;
pub mod host_descriptor;
mod naming;
pub mod pass;
pub mod path_display;
// The `Pipeline` trait the two binaries' front-ends impl. Pure — it
// references only `package_collection` core types and the phase AST — so it
// stays core; the fs-coupled compile drivers that *use* it live in
// the `cli`-gated `cmd` layer.
pub mod backends;
pub mod pipeline;
pub mod pretty;
// The lexical scope walk over the Surface AST — the shared "what names
// are in scope at this byte offset?" candidate source behind LSP and
// REPL completion. Pure over the always-compiled AST, so it stays core.
pub mod scope_walk;
pub mod sig;
pub mod span;
pub mod tokens;

// Driver / IO layer (`cli` feature). These modules carry the fs /
// process / time the pure eval core does not: `cache` reads and writes
// files and hashes with blake3; `cmd/*` calls `process::exit` /
// `env::current_dir`. A `--no-default-features --features surface`
// build (and every `wasm32-unknown-unknown` build) drops them
// entirely, leaving only the parse → typecheck → elaborate → eval
// pipeline. Gated together with `crate::run` below.
#[cfg(feature = "cli")]
pub mod cache;
#[cfg(feature = "cli")]
pub mod cmd;

// Full-surface support modules, including shared compile-time normalization
// and the source adapters that prepare its input.
#[cfg(all(feature = "surface", feature = "cli"))]
pub mod kiodoc;
#[cfg(feature = "surface")]
pub mod normalization;
// In-memory normalization for embedding hosts. Reachable with `surface`
// alone — no `cli` / `repl` — because it feeds source into the pipeline in
// memory and never links the fs/process driver layer.
#[cfg(feature = "surface")]
pub mod normalize_source;

// Prime modules. `prime::typer`, `prime::canonical`, and the pure strict-Kio'
// lowering walk are shared: the
// full surface compiler validates and canonicalizes the `Lowered ->
// Prime` substitution output, and the kio-prime pipeline uses the same
// validator and statement-spine normalizer after `prime::lower`.
// `prime::pipeline` stays behind the `prime` feature so the kio-prime driver
// remains out of the surface-only compiler slice.
pub mod prime;

// LSP server scaffold (`kio lsp`). Diagnostics-on-save over
// stdio with `lsp-server` + `lsp-types`. The full pipeline is
// the analysis backend, so the LSP feature only makes sense
// when `full` is also on; the `lsp` cargo feature gates the
// module and the `kio lsp` subcommand alike.
#[cfg(all(feature = "surface", feature = "lsp", feature = "cli"))]
pub mod lsp;

// Shared REPL inspector core. This excludes terminal dependencies so
// browser wrappers can reuse command parsing and session state without
// reedline / notify.
#[cfg(feature = "repl-core")]
pub mod repl_core;

// Module inspector (`kio repl`). An interactive prompt over `repl_core`.
// The `repl` cargo feature implies `repl-core`, so the terminal UI and
// wasm wrapper share command/session behavior.
#[cfg(feature = "repl")]
pub mod repl;

pub use exit_code::ExitCode;

/// Base GitHub URL used by `--help` output when linking the
/// authoritative specs. Pinned to the current release tag so the
/// links a user sees in their terminal never drift out from under
/// the `kio` binary they're running. Mirrors the repo-wide version
/// number — `ci/checks/repo-lint/version-check.sh` extracts the
/// tag version (`releases/v0.1.0`) from this constant and asserts it agrees
/// with the manifest versions across the tree.
pub const KIO_DOCS_BASE_URL: &str = "https://github.com/jdevuyst/kio/blob/releases/v0.1.0";

// The usage / help text and `run` dispatch are the CLI surface; they
// share the `cli` gate (a host embedding the eval core uses the lib API,
// not `run`).
#[cfg(feature = "cli")]
const USAGE_KIO: &str = "\
Usage: kio <subcommand> [args]

Subcommands:
  init [<package-name>]       Scaffold a new package in the current
                              directory.
  check                       Typecheck the current package.
  build [--skip-unsupported-targets] [<target-id>...]
                              Transpile to one or more compilation targets.
                              `--skip-unsupported-targets` (no positional
                              ids) skips targets whose backend isn't built
                              into this `kio` instead of erroring.
  fmt [<path>...]             Format Kio source files in place.
  test                        Discharge every `equiv` declaration in the
                              current package via partial evaluation.
  sig [<subcommand>]          Record and gate the package's versioned
                              contract changelog (`*.sig.kio`). Bare
                              `sig` prints a non-mutating status summary;
                              `sig stage` records a compatible delta into
                              the draft (`--force` records a break);
                              `sig commit` seals + increments the version;
                              `sig status` is the CI gate (exit 80/81/82);
                              `sig log` pretty-prints the changelog;
                              `sig compact <version>` collapses additive
                              pre-`<version>` history.
  doc <subcommand>            Validate and render the package's Kiodoc.
                              `doc check` validates Kiodoc
                              directives and embedded snippets;
                              `doc fmt` formats formattable Markdown
                              Kiodoc snippets;
                              `doc build` validates, then renders an
                              HTML / Markdown documentation site.
  cache <subcommand>          Manage the package's Kio-semantic on-disk
                              caches. `cache clear` removes the
                              contents of the build block's
                              `cache \"<path>\";` directory, `cache gc`
                              prunes stale semantic entries, and
                              `cache path` prints the resolved root.
  dep <subcommand>            Manage the package's declared dependencies
                              (`<local>.dep.kio`). `dep fetch` materializes
                              them without building (honoring existing
                              locks); `dep update` re-pins git dependencies
                              by re-resolving each `ref` and rewriting its
                              `<local>.lock.kio`.
  completions <shell>         Print a shell completion script to stdout.
                              `<shell>` is one of `bash`, `zsh`, `fish`.
                              Install per your shell's convention.";

// Help line appended only when the `lsp` feature is on. The kio
// binary still builds with `--features surface` alone (no lsp) for
// hosts that don't want the LSP dependency; the help text reflects
// what's actually wired up so a user invoking `kio lsp` against a
// lsp-less build sees the same "unknown subcommand" path other
// gated subcommands do.
#[cfg(all(feature = "cli", feature = "lsp"))]
const USAGE_KIO_LSP: &str = "
  lsp                         Run the language server (JSON-RPC over
                              stdio). Publishes diagnostics and serves
                              hover, goto, references, completion,
                              symbols, folding, formatting, semantic
                              tokens, and rename.";
#[cfg(all(feature = "cli", not(feature = "lsp")))]
const USAGE_KIO_LSP: &str = "";

// Help line appended only when the `repl` feature is on — same
// gating rationale as `USAGE_KIO_LSP`.
#[cfg(all(feature = "cli", feature = "repl"))]
const USAGE_KIO_REPL: &str = "
  repl [<selector>...]        Open the module inspector — an interactive
                              prompt for loading package modules and
                              querying types, docs, and cross-references.
                              Selectors (module name like `op/main`, or
                              filename) restrict the initial load to a
                              subset; with no selector, every regular
                              module is loaded.";
#[cfg(all(feature = "cli", not(feature = "repl")))]
const USAGE_KIO_REPL: &str = "";

#[cfg(feature = "cli")]
const USAGE_KIO_TAIL_TEMPLATE: &str = "

Options:
  --no-cache                  Disable the Kio-semantic on-disk caches
                              (package-check, typed-module,
                              enriched-IR, emit/artifact, equiv,
                              Kiodoc snippet)
                              for this invocation — both reads and
                              writes. Useful when iterating on the
                              compiler logic where the version-based
                              key inputs can't catch logic drift. The
                              rlib cache is unaffected.
  -h, --help                  Show this help and exit.
  -V, --version               Show version information and exit.

See {base}/specs/cli.md for full command behavior.";

#[cfg(feature = "cli")]
const USAGE_KIO_PRIME_TEMPLATE: &str = "\
Usage: kio-prime <subcommand> [args]

The kio-prime binary is a Kio'-only compiler: every subcommand parses
the input as Kio and rejects any surface-only form (tuple literals,
`if`/`else`, label-value / braced label type forms, `iso!` / `into!` / `onto!` /
`align!` / `ease!` / `atom!` / `match!` / UFCS, `.stem. { ... }`, `literal`,
`labels`, `op`, `equiv`) with a parse error. Pass-through Kio' programs
typecheck and build identically to the `kio` binary.

Subcommands:
  check                       Typecheck the current package.
  build [--skip-unsupported-targets] [<target-id>...]
                              Transpile to one or more compilation targets.
  fmt [<path>...]             Format Kio' source files in place.
  cache <subcommand>          Manage the package's Kio-semantic on-disk
                              caches — see `kio --help`.
  dep <subcommand>            Manage the package's declared dependencies
                              (`dep fetch` / `dep update`) — see
                              `kio --help`.

Options:
  --no-cache                  Disable the Kio-semantic on-disk caches
                              for this invocation — see `kio --help`.
  -h, --help                  Show this help and exit.
  -V, --version               Show version information and exit.

See {base}/specs/cli.md for the underlying subcommand behavior.";

/// The worker-thread stack size, in bytes, that [`run_on_worker`]
/// allocates for the compiler's recursive passes. The default is large
/// because several passes (the recursive-descent parser, structural
/// recovery, the Kio'-source pretty-printer) recurse once per level of
/// AST nesting, and a deeply nested but perfectly legal program — e.g. a
/// `match!` / `if` / `do` body nested hundreds of levels deep — would
/// otherwise overflow a default ~8 MiB stack and abort the process. The
/// size is virtual address space; only touched pages cost real memory,
/// so a shallow program pays nothing. Override with
/// `KIO_STACK_SIZE_MB=<n>` (e.g. for a constrained sandbox).
#[cfg(feature = "cli")]
const DEFAULT_WORKER_STACK_BYTES: usize = 1024 * 1024 * 1024;

/// Resolve the worker / rayon stack size, honoring `KIO_STACK_SIZE_MB`.
#[cfg(feature = "cli")]
fn worker_stack_bytes() -> usize {
    std::env::var("KIO_STACK_SIZE_MB")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|mb| *mb > 0)
        .map(|mb| mb * 1024 * 1024)
        .unwrap_or(DEFAULT_WORKER_STACK_BYTES)
}

/// Run [`run`] on a dedicated worker thread with a large stack so that
/// deeply nested (but legal) programs compile instead of overflowing the
/// default thread stack. The compiler's recursive passes are bounded by
/// the *depth* of the input's AST nesting, not by any fixed limit; the
/// only thing standing between "compiles" and "stack-overflow abort" is
/// available stack, so the binary entry points route through here.
///
/// Under the `parallel` feature this also pins rayon's global thread
/// pool to the same stack size — the per-module fan-outs (parse, recover,
/// the cache-key Kio' emit) run *inside* rayon workers, whose default
/// stack is smaller still, so the deep-nesting recursion can land there
/// too. The pool is configured once, before any fan-out spins it up.
#[cfg(feature = "cli")]
pub fn run_on_worker(args: &[String], prime_only: bool) -> ExitCode {
    let stack = worker_stack_bytes();

    #[cfg(feature = "parallel")]
    {
        // Best-effort: if the global pool was already built (it isn't,
        // this early), the error is ignored and rayon keeps its default.
        let _ = rayon::ThreadPoolBuilder::new()
            .stack_size(stack)
            .build_global();
    }

    let args: Vec<String> = args.to_vec();
    let handle = std::thread::Builder::new()
        .name("kio-main".to_owned())
        .stack_size(stack)
        .spawn(move || run(&args, prime_only))
        .expect("spawn kio worker thread");
    // A panic inside the worker propagates here; resume it so the
    // process aborts with the original message rather than swallowing it.
    match handle.join() {
        Ok(code) => code,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// The `-V` / `--version` line: `kio 0.1.0` for a release build, or
/// `kio 0.1.0-dev (a5163989)` for a build ahead of the release tag —
/// `-dirty` when the working tree has uncommitted tracked changes. The
/// provenance suffix comes from `KIO_GIT_INFO` (set by `build.rs`); it is
/// empty for a release build or a git-less build from a published crate,
/// where the bare version prints. `kio-prime` reports its own name.
#[cfg(feature = "cli")]
fn version_line(prime_only: bool) -> String {
    let name = if prime_only { "kio-prime" } else { "kio" };
    let base = env!("CARGO_PKG_VERSION");
    match option_env!("KIO_GIT_INFO") {
        Some(info) if !info.is_empty() => format!("{name} {base}-dev ({info})"),
        _ => format!("{name} {base}"),
    }
}

/// Top-level dispatch. `prime_only` toggles Kio'-only mode (the
/// `kio-prime` binary always sets this; the `kio` binary never does).
///
/// Gated behind `cli`: it dispatches into the driver `cmd::*` modules
/// (and `kiodoc` / `lsp` / `repl`), which a `--no-default-features
/// --features surface` build does not compile. A host embedding the pure
/// eval core uses the lib API directly instead of `run`.
#[cfg(feature = "cli")]
pub fn run(args: &[String], prime_only: bool) -> ExitCode {
    // `--no-cache` is a global flag the caller may pass anywhere in
    // argv. Strip it before subcommand dispatch and latch the
    // per-process cache-policy flag so every cache-resolution call
    // site downstream sees [`cache::policy::caches_enabled`] return
    // `false`. Idempotent: multiple occurrences collapse to one
    // latch and a single argv strip.
    let owned_args: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--no-cache")
        .cloned()
        .collect();
    if owned_args.len() != args.len() {
        cache::policy::disable_for_process();
    }
    let args: &[String] = &owned_args;

    // The kio binary's usage text is assembled at runtime so the
    // `lsp` subcommand line appears only when the `lsp` feature is
    // on. `USAGE_KIO_LSP` resolves to `""` otherwise. The `{base}`
    // placeholder in the tail templates is substituted with the
    // pinned-release docs URL — see [`KIO_DOCS_BASE_URL`].
    let base = KIO_DOCS_BASE_URL;
    let usage_owned: String = if prime_only {
        USAGE_KIO_PRIME_TEMPLATE.replace("{base}", base)
    } else {
        let tail = USAGE_KIO_TAIL_TEMPLATE.replace("{base}", base);
        format!("{USAGE_KIO}{USAGE_KIO_LSP}{USAGE_KIO_REPL}{tail}")
    };
    let usage: &str = &usage_owned;

    let Some(first) = args.first() else {
        eprintln!("{usage}");
        return ExitCode::Usage;
    };

    let code = match first.as_str() {
        "-h" | "--help" => {
            println!("{usage}");
            ExitCode::Success
        }
        "-V" | "--version" => {
            println!("{}", version_line(prime_only));
            ExitCode::Success
        }
        "init" => {
            if prime_only {
                eprintln!("error: unknown subcommand: init");
                eprintln!();
                eprintln!("{usage}");
                ExitCode::Usage
            } else {
                cmd::init::run(&args[1..])
            }
        }
        "check" => cmd::check::run(&args[1..], prime_only),
        "build" => cmd::build::run(&args[1..], prime_only),
        "fmt" => cmd::fmt::run(&args[1..], prime_only),
        "cache" => cmd::cache::run(&args[1..]),
        // Dependency management is shared by both binaries, like `build` /
        // `check` / `cache`: materialization is a command-level fetch /
        // re-root / pin step that observes no surface-vs-Kio' distinction
        // (a `kio-prime` build already materializes git dependencies the
        // same way), so `kio-prime dep` runs the identical resolver.
        "dep" => cmd::dep::run(&args[1..]),
        #[cfg(feature = "surface")]
        "test" => cmd::test::run(&args[1..], prime_only),
        #[cfg(feature = "surface")]
        "sig" => {
            if prime_only {
                eprintln!("error: unknown subcommand: sig");
                eprintln!();
                eprintln!("{usage}");
                ExitCode::Usage
            } else {
                cmd::sig::run(&args[1..])
            }
        }
        #[cfg(feature = "surface")]
        "doc" => {
            if prime_only {
                eprintln!("error: unknown subcommand: doc");
                eprintln!();
                eprintln!("{usage}");
                ExitCode::Usage
            } else {
                kiodoc::run(&args[1..])
            }
        }
        #[cfg(all(feature = "surface", feature = "lsp"))]
        "lsp" => {
            if prime_only {
                eprintln!("error: unknown subcommand: lsp");
                eprintln!();
                eprintln!("{usage}");
                ExitCode::Usage
            } else {
                lsp::run(&args[1..])
            }
        }
        #[cfg(feature = "repl")]
        "repl" => {
            if prime_only {
                eprintln!("error: unknown subcommand: repl");
                eprintln!();
                eprintln!("{usage}");
                ExitCode::Usage
            } else {
                repl::run(&args[1..])
            }
        }
        // Shell completions are a `kio`-only surface: the Kio'-only
        // `kio-prime` binary has its own (smaller) subcommand set, so
        // a completion script generated for it would advertise the
        // wrong grammar. Same prime-only rejection shape the other
        // kio-only subcommands use.
        "completions" => {
            if prime_only {
                eprintln!("error: unknown subcommand: completions");
                eprintln!();
                eprintln!("{usage}");
                ExitCode::Usage
            } else {
                cmd::completions::run(&args[1..])
            }
        }
        // Internal namespace: not advertised in `usage` (same
        // convention as the `kio-prime` binary). Both the public
        // `kio` and the Kio'-only `kio-prime` route through here so
        // `kio-prime debug tokens` works on Kio'-only sources too —
        // the subcommand is a pure tokenization dump and shares no
        // state with the typer pipeline.
        "debug" => cmd::debug::run(&args[1..]),
        other => {
            eprintln!("error: unknown subcommand: {other}");
            // A near-miss against an advertised subcommand earns a
            // did-you-mean before the full usage. The candidate set
            // mirrors the dispatch arms reachable in this mode/build
            // (the prime binary advertises a smaller set); `debug`
            // stays unadvertised, so it's excluded.
            let mut candidates: Vec<&str> = vec!["check", "build", "fmt", "cache", "dep"];
            if !prime_only {
                candidates.extend(["init", "completions"]);
                #[cfg(feature = "surface")]
                candidates.push("test");
                #[cfg(feature = "surface")]
                candidates.push("sig");
                #[cfg(feature = "surface")]
                candidates.push("doc");
                #[cfg(all(feature = "surface", feature = "lsp"))]
                candidates.push("lsp");
                #[cfg(feature = "repl")]
                candidates.push("repl");
            }
            candidates.sort_unstable();
            if let Some(near) = crate::error::closest_name(other, candidates) {
                eprintln!("did you mean `{near}`?");
            }
            eprintln!();
            eprintln!("{usage}");
            ExitCode::Usage
        }
    };
    if matches!(code, ExitCode::Success) {
        cache::gc::finish_successful_command();
    }
    code
}

// These exercise `run`, so they share its `cli` gate.
#[cfg(all(test, feature = "cli"))]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Empty argv prints usage and returns `Usage` exit. Both modes
    /// share the same shape.
    #[test]
    fn empty_argv_returns_usage_exit() {
        assert_eq!(run(&[], false), ExitCode::Usage);
        assert_eq!(run(&[], true), ExitCode::Usage);
    }

    /// `-h` / `--help` short-circuit before subcommand dispatch and
    /// return `Success` — even with no other args.
    #[test]
    fn help_flag_returns_success() {
        assert_eq!(run(&argv(&["-h"]), false), ExitCode::Success);
        assert_eq!(run(&argv(&["--help"]), false), ExitCode::Success);
        assert_eq!(run(&argv(&["-h"]), true), ExitCode::Success);
        assert_eq!(run(&argv(&["--help"]), true), ExitCode::Success);
    }

    /// `-V` / `--version` short-circuit before subcommand dispatch and
    /// return `Success`, in both binaries.
    #[test]
    fn version_flag_returns_success() {
        assert_eq!(run(&argv(&["-V"]), false), ExitCode::Success);
        assert_eq!(run(&argv(&["--version"]), false), ExitCode::Success);
        assert_eq!(run(&argv(&["-V"]), true), ExitCode::Success);
        assert_eq!(run(&argv(&["--version"]), true), ExitCode::Success);
    }

    /// The version line names the binary and carries the crate version.
    #[test]
    fn version_line_names_binary_and_version() {
        let kio = version_line(false);
        assert!(kio.starts_with("kio "), "unexpected: {kio}");
        assert!(kio.contains(env!("CARGO_PKG_VERSION")), "unexpected: {kio}");

        let prime = version_line(true);
        assert!(prime.starts_with("kio-prime "), "unexpected: {prime}");
        assert!(
            prime.contains(env!("CARGO_PKG_VERSION")),
            "unexpected: {prime}"
        );
    }

    /// An unknown subcommand prints "error: unknown subcommand"
    /// followed by usage, and returns `Usage`. Mode-independent.
    #[test]
    fn unknown_subcommand_returns_usage_exit() {
        assert_eq!(run(&argv(&["nonexistent"]), false), ExitCode::Usage);
        assert_eq!(run(&argv(&["nonexistent"]), true), ExitCode::Usage);
    }

    /// `kio-prime` mode rejects `test` as an unknown subcommand
    /// (the kio-prime binary's slice excludes the test driver). The
    /// kio binary, on the other hand, accepts `test` and routes
    /// through `cmd::test::run`. Smoke-checks the cfg-gated dispatch
    /// without exercising the test driver itself.
    #[cfg(feature = "surface")]
    #[test]
    fn prime_only_mode_rejects_test_subcommand() {
        // No package on disk to test, so the full-mode call would
        // exit with whatever `cmd::test::run` returns for an empty
        // cwd — we don't assert on that. We only assert that the
        // prime-only path takes the "unknown subcommand" branch.
        assert_eq!(run(&argv(&["test"]), true), ExitCode::Usage);
    }
}
