//! Kio' backend — emits one `*.kio` file per source module by formatting the
//! checked pipeline's canonical `Module<Prime>` as Kio' source text. The
//! `Prime` marker guarantees Kio' AST shape; the calling pipeline supplies
//! standalone validation before emission.
//!
//! `Prime` is a strict subset of `Surface` at the AST level — every
//! Prime variant has a same-name `Surface` counterpart, and every
//! surface-only variant is uninhabited at `Prime`
//! (`type Ext = Never`). So the emit pipeline is:
//!
//! 1. Walk the `Module<Prime>` and rebuild it as `Module<Surface>`.
//!    The walk is mechanical: every common variant copies through, and
//!    every surface-only variant arm is a `match *ext {}` proof
//!    (statically dead since the variant is uninhabited at `Prime`).
//! 2. Hand the resulting `Module<Surface>` to [`pretty::pretty_module`]
//!    for canonical formatting.
//!
//! The output is required to be **valid Kio'**. That fact follows from the
//! route-neutral checked `Prime` input: its phase admits no surface-only
//! variants, and every inhabited construct belongs to the shared Kio / Kio'
//! grammar. `ci/infra/kio-prime-check-rs` therefore accepts the emitted regular
//! module files without modification.
//! The package file (including its leading `build { ... }` block)
//! is passed through verbatim (its shape isn't part of the Kio'
//! grammar; it's package boundary syntax).

use crate::ast::{
    CallArg, Expr, Import, ImportItem, ImportKind, Item, Module, ModulePath, PackageFile,
    PathSegment, Prime, Signature, SignatureParam, Surface, Type, convert_meta,
};
use crate::pretty;
use crate::span::Span;
use std::collections::{BTreeSet, HashSet};

/// Format a [`Module<Prime>`] as Kio' source text. The result is
/// valid Kio' — `ci/infra/kio-prime-check-rs` accepts it. Bare same-
/// operator chains are admissible in Kio' (parse-time sugar that
/// folds to the same binary AST), so the regular pretty-printer's
/// canonical chain shape is also Kio'-shaped.
pub fn emit_module(m: &Module<Prime>) -> String {
    let mut canonical = m.clone();
    crate::prime::canonical::canonicalize_module(&mut canonical);
    let mut surface = embed_module(&canonical);
    rewrite_module_qualified_type_paths(&mut surface);
    pretty::pretty_module(&surface)
}

/// Format a [`PackageFile<Prime>`] as Kio package-boundary syntax.
/// The package-file shape (including its leading `build { ... }`
/// block) isn't part of the Kio' grammar; it's reused verbatim across
/// backends. The build block threads through every phase on the AST,
/// so the re-emitted file carries it without a separate copy.
pub fn emit_package_file(e: &PackageFile<Prime>) -> String {
    let surface = embed_package_file(e);
    pretty::pretty_package_file(&surface)
}

// ---- Prime → Surface embed ---------------------------------------------

fn embed_module(m: &Module<Prime>) -> Module<Surface> {
    // Prime carries no trivia; the embedded Surface module's
    // per-item trivia stays empty on each inner node's
    // `meta.leading_trivia` (the default for `Meta::new`).
    let items: Vec<Item<Surface>> = m.items.iter().map(embed_item).collect();
    Module {
        path: m.path.clone(),
        imports: m.imports.clone(),
        items,
        meta: convert_meta(&m.meta),
        doc: m.doc.clone(),
    }
}

// ---- Module-qualified type-path requalification ------------------------

/// Rewrite every **module-path-qualified** type reference in the body
/// (e.g. `testapi.String`) to the spelling that is actually bound in the
/// emitted module, so the re-rendered Kio' re-resolves.
///
/// Reduction-side reflection qualifies every nominal head of a reflected
/// type to its declaring module so compile-time reflection is
/// self-identifying — a consumer's `testapi.String` and a dependency's
/// re-rooted `dep/testapi.String` stay distinct under the identity-exact
/// matcher (see [`crate::normalization`]'s `unfold_qualify_refl_type` and
/// `specs/prime.md`). That qualified path rides through a user-elaborator's
/// reflected type value into the spliced term, so the `Module<Prime>` the
/// kio-prime backend re-renders carries type arguments like
/// `__either__(testapi.String, …)`. JS / TS / Rust / Go erase type
/// arguments, so the qualified path is invisible to them; the kio-prime
/// backend re-renders it as source, where the leading `testapi` is a raw
/// **module path** segment, not a name in scope. The grammar admits
/// `ModulePath '.' TypeName` (`specs/grammar.md` § TypePath), but the
/// resolver only resolves such a path when its head is bound — an `import
/// path/to/mod as alias;` qualified import — so the literal module-segment
/// form fails to re-build with `unbound name testapi`.
///
/// Each qualified path `[m1, …, mn, Leaf]` denotes type `Leaf` of module
/// `m1/…/mn`. The rewrite maps it to the module's binding for that
/// `(module, Leaf)` pair:
///
///   * `import m1/…/mn(Leaf);` already binds bare `Leaf` to that module —
///     render bare `Leaf`.
///   * `import m1/…/mn as A;` binds the alias — render `A.Leaf`.
///   * otherwise the pair is unbound: inject `import m1/…/mn(Leaf);` and
///     render bare `Leaf`. The injected selective import names a module the
///     qualified path already pointed at, so it is a parallel
///     same-direction edge and is open-world-monotonic. When bare `Leaf`
///     is already occupied by a local
///     or imported declaration, the rewrite instead injects a qualified
///     alias `import m1/…/mn as <alias>;` and renders `<alias>.Leaf`, keeping the
///     two bindings distinguishable.
///
/// A single-segment path (`String`) is already a bare name and is left
/// untouched.
fn rewrite_module_qualified_type_paths(module: &mut Module<Surface>) {
    let current_module = module_path_key(&module.path);
    let mut rq = Requalifier::new(module, current_module, module.meta.span);
    for item in &mut module.items {
        rq.item(item);
    }
    module.imports.extend(rq.to_inject);
}

/// Per-module state for [`rewrite_module_qualified_type_paths`]: the
/// name→module bindings the emitted module already carries, plus the
/// `import`s the rewrite injects for previously-unbound qualified pairs.
struct Requalifier {
    /// Bare name → its slash-joined module key, from `import M(Leaf);`.
    selective_leaf_module: std::collections::HashMap<String, String>,
    /// Slash-joined module key → its qualified-import aliases.
    module_aliases: std::collections::HashMap<String, BTreeSet<String>>,
    /// The alias identifiers an `import M as A;` binds. A multi-segment type
    /// path whose head is one of these (`t.Box`) is *already* a valid Kio'
    /// reference — the user wrote it through the alias — and is left
    /// untouched; only paths headed by a raw module-path segment (the
    /// reflection leak) are requalified.
    alias_names: HashSet<String>,
    to_inject: Vec<Import>,
    /// `"<module>\0<leaf>"` keys already covered by an injected selective
    /// import, so a repeated qualified reference injects the `import` once.
    injected_selective: HashSet<String>,
    alias_counter: u32,
    occupied_names: HashSet<String>,
    /// Slash-joined key of the module being emitted. A qualified path
    /// whose module prefix equals this is a same-module reflection leak
    /// (a locally declared head the reduction-side qualifier rooted at
    /// its own module); it de-qualifies to the bare leaf with no `import` —
    /// a same-module `import <self>(Leaf);` would be a self-import that
    /// fails to re-build (`value-level import cycle`).
    current_module: String,
    span: Span,
}

impl Requalifier {
    fn new(module: &Module<Surface>, current_module: String, span: Span) -> Self {
        let mut selective_leaf_module = std::collections::HashMap::new();
        let mut module_aliases: std::collections::HashMap<String, BTreeSet<String>> =
            std::collections::HashMap::new();
        let mut alias_names = HashSet::new();
        let mut occupied_names = HashSet::new();
        for item in &module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                let name = declaration
                    .fn_def()
                    .map(|declaration| &declaration.name)
                    .or_else(|| {
                        declaration
                            .type_alias()
                            .map(|declaration| &declaration.name)
                    })
                    .or_else(|| declaration.newtype().map(|declaration| &declaration.name))
                    .or_else(|| declaration.host_type().map(|declaration| &declaration.name))
                    .or_else(|| declaration.host_fn().map(|declaration| &declaration.name));
                if let Some(name) = name {
                    occupied_names.insert(name.clone());
                }
            });
        }
        for u in &module.imports {
            match &u.kind {
                ImportKind::Selective { items, from } => {
                    let key = module_path_key(from);
                    for name in items.iter().filter_map(ImportItem::as_name) {
                        occupied_names.insert(name.to_owned());
                        selective_leaf_module
                            .entry(name.to_owned())
                            .or_insert_with(|| key.clone());
                    }
                }
                ImportKind::Qualified { path, alias } => {
                    module_aliases
                        .entry(module_path_key(path))
                        .or_default()
                        .insert(alias.clone());
                    alias_names.insert(alias.clone());
                    occupied_names.insert(alias.clone());
                }
                ImportKind::Intrinsics => {
                    occupied_names.extend(
                        crate::pass::resolve::PRIME_INTRINSICS
                            .iter()
                            .map(|name| (*name).to_owned()),
                    );
                }
                ImportKind::Comptime => {
                    occupied_names.extend(
                        crate::comptime::PUBLIC_COMPTIME_NAMES
                            .iter()
                            .map(|name| (*name).to_owned()),
                    );
                }
            }
        }
        Self {
            selective_leaf_module,
            module_aliases,
            alias_names,
            to_inject: Vec::new(),
            injected_selective: HashSet::new(),
            alias_counter: 0,
            occupied_names,
            current_module,
            span,
        }
    }

    fn item(&mut self, item: &mut Item<Surface>) {
        match item {
            Item::FnDef(d) => {
                self.signature(&mut d.sig);
                self.ty(&mut d.ret);
                self.expr(&mut d.body);
            }
            Item::RecGroup(g, _) => {
                for d in &mut g.members {
                    self.signature(&mut d.sig);
                    self.ty(&mut d.ret);
                    self.expr(&mut d.body);
                }
            }
            Item::TypeRecGroup(group) => {
                for member in &mut group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => self.ty(&mut alias.body),
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            self.ty(&mut newtype.payload)
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            for entry in &mut labels.entries {
                                self.ty(&mut entry.payload);
                            }
                            if let Some(arms) = &mut labels.type_alias_arms {
                                for arm in arms {
                                    for entry in &mut arm.entries {
                                        self.ty(&mut entry.payload);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Item::Newtype(d) => self.ty(&mut d.payload),
            Item::TypeAlias(a) => self.ty(&mut a.body),
            Item::HostType(_) => {}
            Item::HostFn(h) => {
                for p in &mut h.params {
                    if let crate::ast::HostFnParam::Value(v) = p {
                        self.ty(&mut v.ty);
                    }
                }
                self.ty(&mut h.ret);
            }
            // Surface-only items, statically absent from a `Prime`-derived
            // module.
            Item::LiteralAlias(..)
            | Item::Labels(..)
            | Item::LabelForward(..)
            | Item::Equiv(..)
            | Item::Elaborator(..)
            | Item::Op(..)
            | Item::VariadicOperator(..) => {}
        }
    }

    fn signature(&mut self, sig: &mut Signature<Surface>) {
        for p in &mut sig.params {
            if let SignatureParam::Value(v) = p
                && let Some(ty) = &mut v.ty
            {
                self.ty(ty);
            }
        }
    }

    fn expr(&mut self, e: &mut Expr<Surface>) {
        match e {
            Expr::Call { callee, args, .. } => {
                self.expr(callee);
                for a in args {
                    match a {
                        CallArg::Type(t) => self.ty(t),
                        CallArg::Value(v) => self.expr(v),
                    }
                }
            }
            Expr::FnExpr {
                sig, ret_ty, body, ..
            } => {
                self.signature(sig);
                if let Some(t) = ret_ty {
                    self.ty(t);
                }
                self.expr(body);
            }
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                self.expr(value);
                self.expr(body);
            }
            // Kio' mandates a `(Type)` annotation on every literal, and
            // the reduction-side reflection qualifier roots that role at
            // its declaring module (`"\n"(testapi.String)`). Requalify it
            // like any other type-path site so it de-qualifies to the
            // bound spelling (`"\n"(String)`); left qualified, the
            // dyn-load-prime interpreter's `make_scalar` rejects the module
            // head as an unknown host type.
            Expr::StrLit { annotation, .. }
            | Expr::IntLit { annotation, .. }
            | Expr::FloatLit { annotation, .. }
            | Expr::BoolLit { annotation, .. } => {
                if let Some(t) = annotation {
                    self.ty(t);
                }
            }
            // `Path` / `Unit` carry no type annotations; surface-only
            // variants are absent from a `Prime`-derived AST.
            _ => {}
        }
    }

    /// Rewrite every module-qualified `Type::Path` head in `ty`, recursing
    /// through arguments, function arrows, products, sums, and `forall`
    /// bodies.
    fn ty(&mut self, ty: &mut Type<Surface>) {
        match ty {
            Type::Path { segments, args, .. } => {
                for a in args.iter_mut() {
                    self.ty(a);
                }
                // A path the user already wrote through a qualified-import
                // alias (`t.Box`) is valid Kio' as-is; only a path headed by
                // a raw module-path segment (the reflection leak) needs
                // requalifying.
                if segments.len() >= 2 && !self.alias_names.contains(segments[0].as_str()) {
                    let leaf = segments.last().expect("len >= 2").name.clone();
                    let module_key = segments[..segments.len() - 1]
                        .iter()
                        .map(PathSegment::as_str)
                        .collect::<Vec<_>>()
                        .join("/");
                    let span = self.span;
                    let new_segments = self.bound_spelling(&module_key, &leaf);
                    *segments = new_segments
                        .into_iter()
                        .map(|n| PathSegment::synth(n, span))
                        .collect();
                }
            }
            Type::Function { param, ret, .. } => {
                self.ty(param);
                self.ty(ret);
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                self.ty(left);
                self.ty(right);
            }
            Type::Forall { body, .. } => self.ty(body),
            Type::Unit { .. }
            | Type::Bottom { .. }
            | Type::Infer { .. }
            | Type::LabelSugar { .. } => {}
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// Resolve a qualified type reference `module_key . leaf` to the
    /// segment list the emitted module binds for it, injecting an `import`
    /// when the pair is not yet bound.
    fn bound_spelling(&mut self, module_key: &str, leaf: &str) -> Vec<String> {
        // The qualified path roots at the module being emitted: a
        // same-module reflection leak (`testapi/main.Box` inside
        // `testapi/main`). The leaf is already in scope as a local
        // declaration → render bare and inject nothing. Injecting a
        // `import <self>(Leaf);` here would be a self-import that fails
        // to re-build (`value-level import cycle`, and `Leaf is not pub`
        // when the local declaration is private).
        if module_key == self.current_module {
            return vec![leaf.to_owned()];
        }
        // Bare `leaf` already binds to this exact module → render bare.
        if self
            .selective_leaf_module
            .get(leaf)
            .is_some_and(|m| m == module_key)
        {
            return vec![leaf.to_owned()];
        }
        // A qualified alias already names this module → render `alias.leaf`.
        if let Some(alias) = self
            .module_aliases
            .get(module_key)
            .and_then(|aliases| aliases.iter().next())
        {
            return vec![alias.clone(), leaf.to_owned()];
        }
        // Unbound pair. If bare `leaf` is free, inject a selective import
        // and render bare. A local declaration occupies the same namespace
        // just as an import does, so it must also force a qualified alias.
        if !self.occupied_names.contains(leaf) {
            if self
                .injected_selective
                .insert(format!("{module_key}\u{0}{leaf}"))
            {
                self.to_inject.push(Import {
                    trailing_trivia: Vec::new(),
                    kind: ImportKind::Selective {
                        items: vec![ImportItem::Name {
                            leading_trivia: Vec::new(),
                            name: leaf.to_owned(),
                            span: self.span,
                        }],
                        from: module_path_from_key(module_key, self.span),
                    },
                    span: self.span,
                    leading_trivia: Vec::new(),
                });
            }
            self.selective_leaf_module
                .insert(leaf.to_owned(), module_key.to_owned());
            self.occupied_names.insert(leaf.to_owned());
            return vec![leaf.to_owned()];
        }
        // Bare `leaf` is already occupied by a local or imported declaration.
        // Bind a fresh qualified alias for this module and render `alias.leaf`.
        let alias = self.fresh_alias();
        self.to_inject.push(Import {
            trailing_trivia: Vec::new(),
            kind: ImportKind::Qualified {
                path: module_path_from_key(module_key, self.span),
                alias: alias.clone(),
            },
            span: self.span,
            leading_trivia: Vec::new(),
        });
        self.module_aliases
            .entry(module_key.to_owned())
            .or_default()
            .insert(alias.clone());
        vec![alias, leaf.to_owned()]
    }

    fn fresh_alias(&mut self) -> String {
        loop {
            let candidate = format!("_q{}", self.alias_counter);
            self.alias_counter += 1;
            if self.occupied_names.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

/// The slash-joined module-path key (`testapi/io`) used to match a
/// qualified type path's module-segment prefix against the `import`-bound
/// modules.
fn module_path_key(path: &ModulePath) -> String {
    path.segments
        .iter()
        .map(PathSegment::as_str)
        .collect::<Vec<_>>()
        .join("/")
}

/// Build a [`ModulePath`] from a slash-joined module key.
fn module_path_from_key(module_key: &str, span: Span) -> ModulePath {
    ModulePath {
        segments: module_key
            .split('/')
            .map(|s| PathSegment::synth(s, span))
            .collect(),
        span,
    }
}

fn embed_package_file(e: &PackageFile<Prime>) -> PackageFile<Surface> {
    PackageFile {
        name: e.name.clone(),
        build: e.build.clone(),
        // `bridge` is phase-independent — carried verbatim.
        bridge: e.bridge.clone(),
        meta: convert_meta(&e.meta),
    }
}

fn embed_item(item: &Item<Prime>) -> Item<Surface> {
    // `convert_item` rebrands every item structurally, including the
    // body-less `HostType` / `HostFn` (opaque in Kio'); canonical
    // statement spines are established on Prime before this embed, so
    // only `FnDef` bodies need the annotation-strip post-pass.
    match crate::ast::convert_item(item) {
        Item::FnDef(mut d) => {
            d.body = strip_prime_let_annotations(d.body);
            Item::FnDef(d)
        }
        other => other,
    }
}

fn strip_prime_let_annotations(e: Expr<Surface>) -> Expr<Surface> {
    match e {
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty,
            body: Box::new(strip_prime_let_annotations(*body)),
            meta,
            caps,
        },
        Expr::Let {
            name,
            name_span,
            value,
            body,
            meta,
            ..
        } => Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty: None,
            pattern: None,
            value: Box::new(strip_prime_let_annotations(*value)),
            body: Box::new(strip_prime_let_annotations(*body)),
            meta,
        },
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(strip_prime_let_annotations(*value)),
            body: Box::new(strip_prime_let_annotations(*body)),
            meta,
        },
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext,
        } => Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(strip_prime_let_annotations(*callee)),
            args: args
                .into_iter()
                .map(|a| match a {
                    CallArg::Type(t) => CallArg::Type(t),
                    CallArg::Value(v) => CallArg::Value(strip_prime_let_annotations(v)),
                })
                .collect(),
            meta,
            ext,
        },
        other => other,
    }
}

// The tests below drive the full Kio pipeline (parse → desugar →
// label_elab → resolve → typecheck → substitute) before handing the
// resulting `Module<Prime>` to `emit_module`. That route is only
// available when the `full` feature is on; the prime-only build skips
// these tests.
#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::Meta;
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::parse;
    use crate::pass::resolve::Package;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    /// Derive the on-disk file path a module must live at from its
    /// declared `module a/b;` path: `a/b.kio` (per the new spec, the
    /// declared segments equal the file's path relative to the
    /// package root; the package name is not prepended), so the
    /// `Package::build` path-coherence check is satisfied with an
    /// empty package root.
    fn module_file_path(module: &crate::ast::Module<crate::ast::Surface>) -> PathBuf {
        let segs = &module.path.segments;
        let mut path = PathBuf::new();
        for seg in &segs[..segs.len().saturating_sub(1)] {
            path.push(&seg.name);
        }
        let stem = segs.last().map(|s| s.name.as_str()).unwrap_or("module");
        path.push(format!("{stem}.kio"));
        path
    }

    /// End-to-end emit a single regular module to Kio' source text.
    /// Drives the full pipeline (parse → desugar → label_elab → resolve
    /// → typecheck → substitute) so the input to `emit_module` is the
    /// actual `Module<Prime>` shape `kio build kio-prime` produces.
    fn emit_one(src: &str) -> String {
        let parsed = parse(src).expect("parse");
        let file_path = module_file_path(&parsed);
        let (lowered_modules, _) =
            FullPipeline::lower_package(vec![(file_path, parsed)], None).expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("check_package");
        let module = &prime.module("x/main").expect("module").module;
        emit_module(module)
    }

    fn compile_sources(sources: &[&str]) -> crate::pass::resolve::Package<crate::ast::Prime> {
        let parsed = sources
            .iter()
            .map(|source| {
                let module = parse(source).expect("parse");
                (module_file_path(&module), module)
            })
            .collect::<Vec<_>>();
        let (lowered_modules, _) =
            FullPipeline::lower_package(parsed, None).expect("lower_package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        check_package(&package).expect("check_package")
    }

    /// Round-trip: a Prime module emitted as Kio' parses back as Kio'
    /// against the formal grammar (and the kio-rs surface parser also
    /// accepts it). The `kio-prime-roundtrip.sh` per-case check
    /// asserts the same property at corpus level; here we exercise the
    /// invariant on a representative input as a tighter unit guard.
    #[test]
    fn emit_module_round_trips_through_kio_surface_parser() {
        let src = "module x/main; \
                   fn id[A](x: A) -> A { x }";
        let out = emit_one(src);
        // Parsing the emitted text must succeed.
        parse(&out).unwrap_or_else(|e| panic!("emitted text failed to re-parse: {e:?}\n{out}"));
        // The output mentions the fn name and one of its parameters.
        assert!(out.contains("fn id"), "missing fn id in: {out}");
        assert!(out.contains("-> A"), "missing -> A in: {out}");
    }

    #[cfg(feature = "prime")]
    #[test]
    fn nominally_grounded_recursive_alias_round_trips_as_fresh_prime() {
        use crate::prime::pipeline::PrimePipeline;

        let out = emit_one(
            "module x/main; \
             rec { \
               type Loop = Box; \
               newtype Box : . | Loop { constructor mk_box; projector un_box; }; \
             } \
             fn alias_is_transparent(value: Loop) -> Box { value }",
        );
        assert!(
            out.contains("rec {"),
            "the recursive scope was erased:\n{out}"
        );
        assert!(
            out.contains("type Loop = Box"),
            "the alias was not emitted:\n{out}"
        );

        let reparsed = parse(&out).unwrap_or_else(|error| {
            panic!("emitted recursive alias failed to parse: {error:?}\n{out}")
        });
        let (fresh_prime, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("x/main.kio"), reparsed)], None)
                .expect("recursive artifact remains Kio'-shaped");
        let package = Package::build(Path::new(""), fresh_prime, None)
            .expect("assemble freshly parsed recursive artifact");
        package
            .resolve_imports()
            .expect("fresh artifact resolves uses");
        package
            .check_in_body_resolution()
            .expect("fresh artifact independently validates recursive scope");
        PrimePipeline::typecheck(&package)
            .expect("fresh Prime validation expands the alias through its nominal anchor");
    }

    #[test]
    fn cross_module_recursive_alias_expands_through_its_nominal_anchor() {
        let prime = compile_sources(&[
            "module x/provider; \
             rec { \
               pub type Loop = Box; \
               pub newtype Box : . | Loop { pub constructor mk_box; pub projector un_box; }; \
             }",
            "module x/main; import x/provider(Loop, Box); \
             fn alias_is_transparent(value: Loop) -> Box { value }",
        ]);
        assert!(prime.module("x/provider").is_some());
        assert!(prime.module("x/main").is_some());
    }

    #[cfg(feature = "prime")]
    #[test]
    fn complete_higher_kinded_function_values_round_trip_as_fresh_prime() {
        use crate::prime::pipeline::PrimePipeline;

        let out = emit_one(
            "module x/main;
             newtype Box[A] : A { constructor mk_box; projector un_box; };
             rec newtype Recursive : . | (([*F] F(.) -> F(.)) & Recursive) {
               constructor mk_recursive; projector un_recursive;
             };
             newtype Existential <U> : U & ([*F] F(U) -> F(U)) {
               constructor mk_existential; projector un_existential;
             };
             fn anonymous(value: Box(.)) -> Box(.) {
               .[*F](item: F(.)) -> F(.) { item }(Box, value)
             }",
        );
        let reparsed = parse(&out).unwrap_or_else(|error| {
            panic!("emitted HKT artifact failed to parse: {error:?}\n{out}")
        });
        let (fresh_prime, _) =
            PrimePipeline::lower_package(vec![(PathBuf::from("x/main.kio"), reparsed)], None)
                .expect("emitted HKT artifact remains Kio'-shaped");
        let package = Package::build(Path::new(""), fresh_prime, None)
            .expect("assemble freshly parsed HKT artifact");
        package
            .resolve_imports()
            .expect("fresh HKT artifact resolves");
        package
            .check_binding_origins()
            .expect("fresh HKT artifact retains binding origins");
        package
            .check_no_value_cycles()
            .expect("fresh HKT artifact has no value cycle");
        package
            .check_in_body_resolution()
            .expect("fresh HKT artifact resolves function bodies");
        PrimePipeline::typecheck(&package)
            .expect("standalone Prime validation accepts structural HKT function values");
    }

    #[cfg(feature = "prime")]
    #[test]
    fn unit_domain_calls_preserve_written_value_boundaries_and_revalidate_as_prime() {
        use crate::prime::pipeline::PrimePipeline;

        let out = emit_one(
            "module x/main; \
             host type T; \
             host fn nil[A]() -> A; \
             fn id[A](value: A) -> A { value } \
             fn later[A](value: A)[B]() -> A { value } \
             fn selected() -> . -> T { nil(T) } \
             fn explicit() -> T { nil(T, ()) } \
             fn selected_via_let() -> . -> T { let f = nil; f(T) } \
             fn selected_later(value: T) -> . -> T { later(T, value)(T) } \
             fn explicit_later(value: T) -> T { later(T, value)(T, ()) } \
             fn residual() -> T -> T { id(T) } \
             fn selected_unit() -> . -> . { id(.) } \
             fn empty() -> . { id() }",
        );
        assert_eq!(
            out.matches("nil(T)").count(),
            1,
            "type-only Unit-domain calls must remain residual in canonical Kio': {out}"
        );
        assert!(
            out.contains("nil(T, ())"),
            "a written Unit value must remain explicit in canonical Kio': {out}"
        );
        assert!(
            out.contains("let f = nil; let _kg0 = f(T); .() { _kg0(()) }"),
            "a locally bound type-only Unit-domain function must remain residual: {out}"
        );
        assert!(
            out.contains("later(T, value)(T)") && out.contains("later(T, value)(T, ())"),
            "later type-only and saturated Unit layers must remain distinct: {out}"
        );
        assert!(
            out.contains("id(T)"),
            "a type-only non-Unit call must remain residual: {out}"
        );
        assert_eq!(
            out.matches("id(.)").count(),
            1,
            "a written type-only Unit call must remain residual: {out}"
        );
        assert_eq!(
            out.matches("id(., ())").count(),
            1,
            "a source-empty call must emit its inferred type and Unit value: {out}"
        );

        let reparsed = parse(&out).expect("emitted Kio' re-parses");
        let file_path = module_file_path(&reparsed);
        let (modules, _) = PrimePipeline::lower_package(vec![(file_path, reparsed)], None)
            .expect("emitted source is Kio'-shaped");
        let package =
            Package::build(Path::new(""), modules, None).expect("rebuild emitted Kio' package");
        package
            .resolve_imports()
            .expect("resolve emitted Kio' names");
        package
            .check_in_body_resolution()
            .expect("resolve emitted Kio' bodies");
        PrimePipeline::typecheck(&package)
            .expect("standalone Prime validation accepts the emitted calls");
    }

    /// `equiv` items don't reach the kio-prime backend: the
    /// substitute pass filters them. `emit_module` therefore must
    /// never produce an `equiv` line in its output.
    #[test]
    fn emit_module_does_not_contain_equiv_after_substitute() {
        let src = "module x/main; \
                   fn id_unit() -> . { () } \
                   equiv id_unit_eq { id_unit(); () }";
        let out = emit_one(src);
        assert!(
            !out.contains("equiv"),
            "equiv leaked into Kio' output: {out}"
        );
        assert!(out.contains("fn id_unit"), "expected fn in: {out}");
    }

    /// `if!`/`else` is elaborated before Prime emission; emit_module sees
    /// the `__if_then_else__` intrinsic call already substituted into
    /// the body. The output contains no `if!` surface call.
    #[test]
    fn if_else_replaced_by_intrinsic_call_in_emitted_output() {
        let src = "module main; import control(if); \
                   host type Bool role(bool); \
                   fn pick(c: Bool) -> . { if! c { () } else { () } }";
        let parsed = parse(src).expect("parse");
        let file_path = module_file_path(&parsed);
        let control = parse(include_str!(
            "../../../test-data/poc/elab/workdir/control.kio"
        ))
        .expect("parse control provider");
        let control_path = module_file_path(&control);
        let (lowered_modules, _) =
            FullPipeline::lower_package(vec![(file_path, parsed), (control_path, control)], None)
                .expect("lower");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("check_package");
        let module = &prime.module("main").expect("module").module;
        let out = emit_module(module);
        assert!(
            out.contains("__if_then_else__"),
            "expected __if_then_else__ call: {out}"
        );
        assert!(!out.contains("if!"), "if! survived: {out}");
        let reparsed =
            parse(&out).unwrap_or_else(|e| panic!("emitted text failed to re-parse: {e:?}\n{out}"));
        crate::prime::lower::lower_module(reparsed)
            .unwrap_or_else(|e| panic!("emitted text is not Kio': {e:?}\n{out}"));
    }

    /// Kio' mandates the `(Type)` annotation on every literal (see
    /// `specs/grammar.md` § Kio' grammar — `LiteralCall ::= LITERAL '('
    /// Type ')'`), so a literal value-arg carries its annotation in the
    /// kio-prime emit regardless of whether a sibling call slot pins
    /// the same type. A call like `id(String, "hi")` round-trips as
    /// `id(String, "hi"(String))` — both pieces of type information
    /// survive, and the emitted text re-parses as Kio'.
    #[test]
    fn literal_value_arg_carries_annotation_alongside_explicit_type_arg() {
        let src = "module main; \
                   host type String role(str); \
                   fn id[A](x: A) -> A { x } \
                   fn use_id() -> String { id(String, \"hi\") }";
        let parsed = parse(src).expect("parse");
        let file_path = module_file_path(&parsed);
        let (lowered_modules, _) =
            FullPipeline::lower_package(vec![(file_path, parsed)], None).expect("lower");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("check_package");
        let module = &prime.module("main").expect("module").module;
        let out = emit_module(module);
        // The explicit `String` type-arg survives.
        assert!(
            out.contains("id(String,"),
            "expected explicit `String` type-arg in: {out}"
        );
        // The literal value-arg carries its mandatory `(String)`
        // annotation — Kio' admits no bare-literal form.
        assert!(
            out.contains("\"hi\"(String)"),
            "expected literal `\"hi\"(String)` annotation in: {out}"
        );
        // The output re-parses as Kio'.
        parse(&out).unwrap_or_else(|e| panic!("emitted text failed to re-parse: {e:?}\n{out}"));
    }

    /// Even when no sibling type-arg pins the literal's type, the
    /// `(Type)` annotation is present in the emit — the Kio' grammar
    /// requires it on every literal.
    #[test]
    fn literal_without_sibling_type_arg_keeps_baked_annotation() {
        let src = "module main; \
                   host type String role(str); host fn print(s: String) -> .; \
                   fn use_print() -> . { print(\"hello\") }";
        let parsed = parse(src).expect("parse");
        let file_path = module_file_path(&parsed);
        let (lowered_modules, _) =
            FullPipeline::lower_package(vec![(file_path, parsed)], None).expect("lower");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("check_package");
        let module = &prime.module("main").expect("module").module;
        let out = emit_module(module);
        // The literal annotation is preserved when no sibling type-arg
        // pins the type (the kio-prime backend's standard literal-
        // explicitness behavior).
        assert!(
            out.contains("\"hello\"(String)"),
            "expected literal `\"hello\"(String)` annotation in: {out}"
        );
    }

    #[test]
    fn checked_lambda_param_type_is_emitted() {
        let src = "module x/main; \
                   labels { greet: . }; \
                   fn id[A](x: A) -> A { x } \
                   fn main() -> . { let f = id(Greet -> ., .(_g) { () }); () }";
        let out = emit_one(src);
        assert!(
            out.contains(".(_g: Greet)"),
            "expected checked lambda parameter annotation in: {out}"
        );
        parse(&out).unwrap_or_else(|e| panic!("emitted text failed to re-parse: {e:?}\n{out}"));
    }

    /// Build a one-`fn` surface module whose `fn r` return type is the
    /// given `ret_ty`, with the given `import`s prepended, so a unit test can
    /// drive [`rewrite_module_qualified_type_paths`] over a constructed
    /// qualified `Type::Path` without standing up a whole package.
    fn module_with_imports_and_return(
        uses_src: &str,
        ret_ty: Type<Surface>,
    ) -> crate::ast::Module<Surface> {
        let src = format!("module testapi/main; {uses_src} fn r() -> . {{ () }}");
        let mut module = parse(&src).expect("parse");
        set_return_type(&mut module, ret_ty);
        module
    }

    fn set_return_type(module: &mut crate::ast::Module<Surface>, ret_ty: Type<Surface>) {
        let def = module
            .items
            .iter_mut()
            .find_map(|item| match item {
                crate::ast::Item::FnDef(def) if def.name == "r" => Some(def),
                _ => None,
            })
            .expect("expected function `r`");
        def.ret = ret_ty;
    }

    fn qualified_path(segments: &[&str]) -> Type<Surface> {
        Type::synth_path(
            segments.iter().map(|s| (*s).to_owned()).collect(),
            Vec::new(),
            Span::new(0, 0),
        )
    }

    fn render_return_type(module: &crate::ast::Module<Surface>) -> Type<Surface> {
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::FnDef(def) if def.name == "r" => Some(def),
                _ => None,
            })
            .expect("expected function `r`");
        def.ret.clone()
    }

    fn type_path_segment_names(ty: &Type<Surface>) -> Vec<String> {
        match ty {
            Type::Path { segments, .. } => segments.iter().map(|s| s.name.clone()).collect(),
            other => panic!("expected a Type::Path, got {other:?}"),
        }
    }

    /// A qualified host-type path whose leaf is already selectively
    /// imported from the qualifying module is de-qualified to the bound
    /// bare name (the `__either__(testapi.String, …)` reflection-leak case
    /// the three prime goldens exercise at corpus level).
    #[test]
    fn qualified_path_to_imported_leaf_dequalifies_to_bare() {
        let mut module = module_with_imports_and_return(
            "import testapi(String);",
            qualified_path(&["testapi", "String"]),
        );
        let before = module.imports.len();
        rewrite_module_qualified_type_paths(&mut module);
        assert_eq!(
            type_path_segment_names(&render_return_type(&module)),
            vec!["String".to_owned()],
            "qualified path should collapse to the bound bare name"
        );
        assert_eq!(
            module.imports.len(),
            before,
            "no `import` should be injected when the leaf is already imported"
        );
    }

    /// A qualified path to a module already bound by `import M as A;` renders
    /// through the alias (`A.Leaf`), not the raw module segments.
    #[test]
    fn qualified_path_through_existing_alias_uses_alias() {
        let mut module = module_with_imports_and_return(
            "import testapi/types as t;",
            qualified_path(&["testapi", "types", "Celsius"]),
        );
        rewrite_module_qualified_type_paths(&mut module);
        assert_eq!(
            type_path_segment_names(&render_return_type(&module)),
            vec!["t".to_owned(), "Celsius".to_owned()],
            "qualified path should re-render through the existing alias"
        );
    }

    /// A path the user already wrote through an alias (`t.Box`, stored with
    /// the alias as its head segment) is valid Kio' as-is and must be left
    /// untouched — the rewrite must not mistake the alias head for a raw
    /// module-path segment and inject a bogus `import t(Box);`. This is the
    /// `exec_qualified_*` golden shape.
    #[test]
    fn alias_headed_path_is_left_untouched() {
        let mut module = module_with_imports_and_return(
            "import testapi/types as t;",
            qualified_path(&["t", "Box"]),
        );
        let before = module.imports.len();
        rewrite_module_qualified_type_paths(&mut module);
        assert_eq!(
            type_path_segment_names(&render_return_type(&module)),
            vec!["t".to_owned(), "Box".to_owned()],
            "an alias-headed path must round-trip unchanged"
        );
        assert_eq!(
            module.imports.len(),
            before,
            "no `import` should be injected for an alias-headed path"
        );
    }

    /// A qualified path whose `(module, leaf)` pair has no binding yet, and
    /// whose bare leaf name is free, injects an `import M(Leaf);` and
    /// renders bare.
    #[test]
    fn qualified_path_unbound_injects_selective_import_and_dequalifies() {
        let mut module =
            module_with_imports_and_return("", qualified_path(&["testapi", "fmt", "Doc"]));
        rewrite_module_qualified_type_paths(&mut module);
        assert_eq!(
            type_path_segment_names(&render_return_type(&module)),
            vec!["Doc".to_owned()],
        );
        let injected = module.imports.iter().any(|u| {
            matches!(&u.kind, ImportKind::Selective { items, from }
                if module_path_key(from) == "testapi/fmt"
                    && items.iter().filter_map(ImportItem::as_name).any(|n| n == "Doc"))
        });
        assert!(injected, "expected an injected `import testapi/fmt(Doc);`");
        // The emitted module re-renders to source that re-parses.
        let out = pretty::pretty_module(&module);
        parse(&out).unwrap_or_else(|e| panic!("emitted text failed to re-parse: {e:?}\n{out}"));
    }

    /// When the bare leaf name is already taken by a *different* module (the
    /// same-named host-type collision the typer's qualification exists to
    /// keep apart), the rewrite cannot de-qualify to bare; it binds a fresh
    /// qualified alias for the second module and renders `alias.Leaf`, so
    /// the two same-named host types stay distinguishable in the output.
    #[test]
    fn qualified_path_colliding_leaf_binds_fresh_alias() {
        // Bare `String` is bound to `testapi`; a path to `dep/testapi`'s
        // own `String` cannot reuse the bare name.
        let mut module = module_with_imports_and_return(
            "import testapi(String);",
            qualified_path(&["dep", "testapi", "String"]),
        );
        rewrite_module_qualified_type_paths(&mut module);
        let segs = type_path_segment_names(&render_return_type(&module));
        assert_eq!(
            segs.len(),
            2,
            "expected an `alias.String` path, got {segs:?}"
        );
        assert_eq!(segs[1], "String");
        let alias = &segs[0];
        assert_ne!(alias, "String");
        let alias_use = module.imports.iter().any(|u| {
            matches!(&u.kind, ImportKind::Qualified { path, alias: a, .. }
                if a == alias && module_path_key(path) == "dep/testapi")
        });
        assert!(
            alias_use,
            "expected an injected `import dep/testapi as {alias};`"
        );
        let out = pretty::pretty_module(&module);
        parse(&out).unwrap_or_else(|e| panic!("emitted text failed to re-parse: {e:?}\n{out}"));
    }

    #[cfg(feature = "prime")]
    #[test]
    fn qualified_path_colliding_with_local_type_binds_fresh_alias() {
        let mut module = parse("module testapi/main; type Doc = .; fn r() -> . { () }")
            .expect("parse module with local type");
        set_return_type(&mut module, qualified_path(&["testapi", "foreign", "Doc"]));

        rewrite_module_qualified_type_paths(&mut module);

        let segments = type_path_segment_names(&render_return_type(&module));
        assert_eq!(segments.len(), 2, "expected an `alias.Doc` path");
        assert_eq!(segments[1], "Doc");
        let alias = &segments[0];
        assert!(module.imports.iter().any(|import_| {
            matches!(&import_.kind, ImportKind::Qualified { path, alias: bound }
                if bound == alias && module_path_key(path) == "testapi/foreign")
        }));
        assert!(!module.imports.iter().any(|import_| {
            matches!(&import_.kind, ImportKind::Selective { items, .. }
                if items.iter().filter_map(ImportItem::as_name).any(|name| name == "Doc"))
        }));

        let emitted = pretty::pretty_module(&module);
        let reparsed = [
            parse(&emitted).expect("reparse rewritten main module"),
            parse("module testapi/foreign; pub type Doc = .;").expect("parse referenced module"),
        ]
        .into_iter()
        .map(|module| (module_file_path(&module), module))
        .collect();
        let (fresh_prime, _) = crate::prime::pipeline::PrimePipeline::lower_package(reparsed, None)
            .expect("rewritten package is Kio'-shaped");
        let package = Package::build(Path::new(""), fresh_prime, None)
            .expect("assemble freshly parsed Kio' package");
        package
            .resolve_imports()
            .expect("generated qualified import resolves");
        package
            .check_binding_origins()
            .expect("local and foreign `Doc` retain distinct binding origins");
        package
            .check_no_value_cycles()
            .expect("generated qualified import does not introduce a cycle");
        package
            .check_in_body_resolution()
            .expect("rewritten function signature resolves");
        crate::prime::pipeline::PrimePipeline::typecheck(&package)
            .expect("standalone Prime validation accepts the rewritten package");
    }

    #[cfg(feature = "prime")]
    #[test]
    fn generated_qualified_alias_avoids_local_item_and_round_trips_to_prime() {
        use crate::prime::pipeline::PrimePipeline;

        let parsed = [
            ("left.kio", "module left; pub host type Shared;"),
            ("right.kio", "module right; pub host type Shared;"),
            (
                "main.kio",
                "module main; \
                 import left(Shared); \
                 fn _q0() -> . { () } \
                 fn keep(value: right.Shared) -> right.Shared { value }",
            ),
        ]
        .into_iter()
        .map(|(path, source)| {
            (
                PathBuf::from(path),
                parse(source).unwrap_or_else(|error| panic!("parse `{path}`: {error:?}")),
            )
        })
        .collect();
        let (unchecked_prime, _) = PrimePipeline::lower_package(parsed, None)
            .expect("the synthetic reflection-qualified artifact is Kio'-shaped");

        let mut emitted_main = None;
        let reparsed = unchecked_prime
            .into_iter()
            .map(|(path, module)| {
                let source = emit_module(&module);
                if path == Path::new("main.kio") {
                    emitted_main = Some(source.clone());
                }
                (
                    path.clone(),
                    parse(&source).unwrap_or_else(|error| {
                        panic!(
                            "parse emitted `{}` as fresh source: {error:?}\n{source}",
                            path.display()
                        )
                    }),
                )
            })
            .collect();
        let emitted_main = emitted_main.expect("emitted package must contain module `main`");

        let (fresh_prime, _) = PrimePipeline::lower_package(reparsed, None)
            .expect("fresh source is admitted by the Kio' grammar");
        let fresh_package = Package::build(Path::new(""), fresh_prime, None)
            .expect("assemble freshly parsed Kio' package");
        fresh_package
            .resolve_imports()
            .expect("fresh Kio' ordinary imports resolve");
        fresh_package
            .check_binding_origins()
            .expect("the generated alias and local item retain distinct binding origins");
        fresh_package
            .check_no_value_cycles()
            .expect("fresh Kio' has no value cycle");
        fresh_package
            .check_in_body_resolution()
            .expect("fresh Kio' resolves both rewritten alias references");
        PrimePipeline::typecheck(&fresh_package)
            .expect("standalone Prime validation accepts the fresh artifact");
        assert!(
            emitted_main.contains("fn _q0"),
            "the occupied first alias candidate must remain a function: {emitted_main}"
        );
        assert!(
            emitted_main.contains("import right as _q1;"),
            "the generated module alias must skip the occupied `_q0`: {emitted_main}"
        );
        assert_eq!(
            emitted_main.matches("_q1.Shared").count(),
            2,
            "the alias declaration must govern both rewritten type references: {emitted_main}"
        );
    }

    #[cfg(feature = "prime")]
    #[test]
    fn alias_owner_is_stable_when_an_imported_provider_gains_the_same_leaf() {
        use crate::prime::pipeline::PrimePipeline;

        const OWNER: &str = "module owner; \
             pub newtype Shared : . { \
               pub constructor mk_shared; \
               pub projector un_shared; \
             }; \
             pub type Alias = Shared;";
        const CONSUMER: &str = "module main; \
             import owner(Alias); \
             import noise(touch); \
             import __intrinsics__; \
             fn widen(value: Alias) -> Alias | . { \
               touch(); \
               __left__(_, ., value) \
             }";
        const NOISE_BASELINE: &str = "module noise; pub fn touch() -> . { () }";
        const UNRELATED_DECLARATION: &str = "pub newtype Shared : . { \
               pub constructor mk_shared; \
               pub projector un_shared; \
             };";

        fn compile(noise: &str) -> Package<Prime> {
            let parsed = [OWNER, CONSUMER, noise]
                .into_iter()
                .map(|source| {
                    let module = parse(source).expect("parse source package");
                    (module_file_path(&module), module)
                })
                .collect();
            let (lowered_modules, _) =
                FullPipeline::lower_package(parsed, None).expect("lower source package");
            let package = Package::build(Path::new(""), lowered_modules, None)
                .expect("assemble source package");
            package.resolve_imports().expect("resolve source imports");
            package
                .check_binding_origins()
                .expect("source bindings retain their owners");
            package
                .check_no_value_cycles()
                .expect("source package has no value cycle");
            package
                .check_in_body_resolution()
                .expect("resolve source bodies");
            check_package(&package).expect("typecheck source package")
        }

        fn emit_and_recheck(
            package: &Package<Prime>,
        ) -> std::collections::BTreeMap<String, String> {
            let mut emitted = std::collections::BTreeMap::new();
            let reparsed = package
                .modules()
                .map(|(module_path, entry)| {
                    let source = emit_module(&entry.module);
                    emitted.insert(module_path.to_owned(), source.clone());
                    (
                        PathBuf::from(format!("{module_path}.kio")),
                        parse(&source).unwrap_or_else(|error| {
                            panic!("fresh parse of `{module_path}` failed: {error:?}\n{source}")
                        }),
                    )
                })
                .collect();
            let (fresh_prime, _) = PrimePipeline::lower_package(reparsed, None)
                .expect("emitted package remains Kio'-shaped");
            let fresh = Package::build(Path::new(""), fresh_prime, None)
                .expect("assemble freshly parsed Kio' package");
            fresh.resolve_imports().expect("fresh Kio' imports resolve");
            fresh
                .check_binding_origins()
                .expect("fresh Kio' retains exact nominal owners");
            fresh
                .check_no_value_cycles()
                .expect("fresh Kio' has no value cycle");
            fresh
                .check_in_body_resolution()
                .expect("fresh Kio' bodies resolve");
            PrimePipeline::typecheck(&fresh)
                .expect("standalone Prime validation accepts the fresh artifact");
            emitted
        }

        let baseline = compile(NOISE_BASELINE);
        let grown_noise = format!("{NOISE_BASELINE} {UNRELATED_DECLARATION}");
        let grown = compile(&grown_noise);
        let baseline_main = &baseline.module("main").expect("baseline main").module;
        let grown_main = &grown.module("main").expect("grown main").module;
        let owner_alias = baseline_main
            .imports
            .iter()
            .find_map(|import_| match &import_.kind {
                ImportKind::Qualified { path, alias } if module_path_key(path) == "owner" => {
                    Some(alias.clone())
                }
                _ => None,
            })
            .expect("pre-emission Prime must carry an exact owner binding");
        assert_eq!(
            postcard::to_allocvec(baseline_main).expect("encode baseline Prime module"),
            postcard::to_allocvec(grown_main).expect("encode grown Prime module"),
            "an unrelated declaration must not perturb the pre-emission Prime consumer"
        );

        let baseline_emitted = emit_and_recheck(&baseline);
        let grown_emitted = emit_and_recheck(&grown);
        let baseline_main = baseline_emitted.get("main").expect("emitted baseline main");
        let grown_main = grown_emitted.get("main").expect("emitted grown main");
        assert_eq!(
            baseline_main, grown_main,
            "an unrelated same-leaf newtype must not perturb emitted Kio'"
        );
        assert!(
            baseline_main.contains(&format!("import owner as {owner_alias};"))
                && baseline_main.contains(&format!("{owner_alias}.Shared")),
            "the alias's resolved owner must be explicit in emitted Kio': {baseline_main}"
        );
        assert!(
            !parse(baseline_main)
                .expect("parse emitted baseline imports")
                .imports
                .iter()
                .any(|import| {
                    matches!(&import.kind, ImportKind::Selective { items, .. }
                        if items.iter().filter_map(ImportItem::as_name).any(|name| name == "Shared"))
                }),
            "emission must not rediscover a nominal owner from a bare leaf: {baseline_main}"
        );
    }

    /// A qualified path whose module prefix equals the module being
    /// emitted (`testapi/main.Box` inside `testapi/main`) is a
    /// same-module reflection leak: the reduction-side qualifier roots a
    /// locally declared head at its own module. It must de-qualify to the
    /// bare leaf with no injected `import`. An `import testapi/main(Box);`
    /// here would be a self-import that fails to re-build with
    /// `value-level import cycle: module testapi/main already on the path to
    /// testapi/main` (and `Box is not pub` for a private local head) —
    /// the `exec_poly_monad_dictionary` / `exec_monad_*` golden shape.
    #[test]
    fn qualified_path_to_own_module_dequalifies_to_bare_without_self_import() {
        let mut module =
            module_with_imports_and_return("", qualified_path(&["testapi", "main", "Box"]));
        let before = module.imports.len();
        rewrite_module_qualified_type_paths(&mut module);
        assert_eq!(
            type_path_segment_names(&render_return_type(&module)),
            vec!["Box".to_owned()],
            "a same-module qualified path should collapse to the bare local name"
        );
        assert_eq!(
            module.imports.len(),
            before,
            "no `import` should be injected for a same-module qualified path"
        );
        let self_import = module.imports.iter().any(|u| {
            matches!(&u.kind, ImportKind::Selective { from, .. } if module_path_key(from) == "testapi/main")
        });
        assert!(
            !self_import,
            "a same-module path must not inject an `import testapi/main(...);` self-import"
        );
    }

    /// A literal's mandatory `(Type)` annotation is a type-path site too:
    /// the reduction-side qualifier roots the role at its declaring module
    /// (`"hi"(testapi.String)`), so the requalifier must de-qualify it to
    /// the bound spelling (`"hi"(String)`) like any other type path. Left
    /// qualified, the dyn-load-prime interpreter's `make_scalar` rejects the
    /// module head with `unknown host type testapi` — the empty-stdout
    /// failure the `exec_prime_eval_*` goldens hit at corpus level.
    #[test]
    fn qualified_literal_annotation_dequalifies_to_bound_role() {
        let mut module =
            module_with_imports_and_return("import testapi(String);", qualified_path(&["String"]));
        let crate::ast::Item::FnDef(def) = &mut module.items[0] else {
            panic!("expected a function item")
        };
        def.body = Expr::StrLit {
            occurrence: Default::default(),
            value: "hi".to_owned(),
            annotation: Some(qualified_path(&["testapi", "String"])),
            meta: Meta::new(Span::new(0, 0)),
        };
        let before = module.imports.len();
        rewrite_module_qualified_type_paths(&mut module);
        let crate::ast::Item::FnDef(def) = &module.items[0] else {
            panic!("expected a function item")
        };
        let Expr::StrLit { annotation, .. } = &def.body else {
            panic!("expected a string-literal body, got {:?}", def.body)
        };
        let ann = annotation.as_ref().expect("annotation present");
        assert_eq!(
            type_path_segment_names(ann),
            vec!["String".to_owned()],
            "a qualified literal annotation should collapse to the bound bare role"
        );
        assert_eq!(
            module.imports.len(),
            before,
            "de-qualifying to an already-imported role injects no `import`"
        );
        let out = pretty::pretty_module(&module);
        assert!(
            out.contains("\"hi\"(String)") && !out.contains("testapi.String"),
            "expected `\"hi\"(String)` with no qualified role in: {out}"
        );
        parse(&out).unwrap_or_else(|e| panic!("emitted text failed to re-parse: {e:?}\n{out}"));
    }
}
