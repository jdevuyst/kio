//! Surface → Prime lowering for the kio-prime binary.
//!
//! Single function [`lower_module`] (and [`lower_package_file`]) that
//! walks a parsed Surface AST once, rejects every surface-only
//! variant with a parse error, and produces a Prime AST directly —
//! skipping the elaboration-bearing `Lowered` phase that the full
//! pipeline goes through.
//!
//! The walk rejects every Surface extension that is uninhabited at Prime,
//! including:
//!
//! - `Expr::Tuple` — n-ary tuple literal sugar.
//! - `Expr::FnPlaceholder`, `Expr::OpChain`, and recursive-call annotations.
//! - `Expr::BlockCall` — calls with trailing blocks.
//! - `Expr::LabelValue` — `{f = e}` label-value sugar.
//! - `Expr::Elaborator` — field access (`x.?{foo}`) and field update
//!   (`x.!{foo = y}`). Any
//!   [`crate::ast::ElaboratorKind`] is rejected.
//! - `Expr::UserElaborator` — reference palettes, `match!`, `derive!`, and
//!   package-defined elaborator calls alike.
//! - `Expr::Ufcs` — regular UFCS (`r.>f`) and elaborator-UFCS
//!   (`r.>iso!(T)`) dispatch.
//! - `Type::LabelSugar` — `{f: T}` label-derived type sugar.
//! - `Item::Labels` — label declarations.
//! - `Item::LiteralAlias` — `literal` declarations.
//! - `Item::Equiv`, `Item::Op`, `Item::Fold`, term-level `Item::RecGroup`, and
//!   user elaborator declarations; operator-pattern and label imports are
//!   rejected for the same reason. `Item::TypeRecGroup` is shared Kio' syntax
//!   and survives this lowering.
//!
//! Each rejection emits an `Error::Parse` whose message names the
//! form and explains "this is not part of Kio'; the kio-prime
//! binary only accepts Kio'".
//!
//! The actual clone-and-recurse boilerplate lives in
//! [`crate::prime::walk::SurfaceToPrime`]; this module's
//! [`PrimeLower`] visitor just spells out the per-variant reject
//! messages.
//!
//! The full Kio route applies operator folding, desugaring, label elaboration,
//! resolution and alpha-normalization, Lowered typechecking, and substitution
//! before it reaches Prime; the shared standalone checker then validates,
//! completes, and canonicalizes that package. This module is the Kio'-only
//! Surface → Prime step: it rejects the surface forms instead, after which the
//! pipeline performs resolution, alpha-normalization, and the same standalone
//! Prime check.

use crate::ast::{
    CallArg, Equiv, Expr, Item, LabelSugarLabel, LabelValueLabel, Labels, LiteralAlias, Meta,
    Module, Op, PackageFile, PathSegment, PlaceholderState, Prime, RecGroup, Surface, Type,
};
use crate::error::Error;
use crate::prime::walk::SurfaceToPrime;
use crate::span::Span;

/// Walk a parsed Surface module once, reject every surface-only
/// variant with `Error::Parse`, and produce a Prime module
/// directly.
pub fn lower_module(module: Module<Surface>) -> Result<Module<Prime>, Error> {
    PrimeLower.walk_module(module)
}

/// Walk a parsed Surface package file. Mirrors [`lower_module`] for
/// the package-file shape: bridge adapter bodies hold expressions
/// that may use surface forms, and those are rejected here too.
pub fn lower_package_file(export: PackageFile<Surface>) -> Result<PackageFile<Prime>, Error> {
    PrimeLower.walk_package_file(export)
}

/// Build the parse error returned when a surface-only variant is
/// encountered. Names the form so the user can locate it; suggests
/// the `kio` binary as the recovery path, since Kio'-only mode is
/// a property of the `kio-prime` binary itself rather than a flag.
fn prime_reject(form: &str, span: Span) -> Error {
    Error::parse(
        span,
        format!(
            "{form} is not part of Kio'. The `kio-prime` binary only accepts Kio'; \
             use the `kio` binary to compile this program as full Kio."
        ),
    )
}

/// Visitor that rejects every surface-only variant with
/// [`prime_reject`]. Inhabited-at-Prime variants pass through via
/// the trait's default clone-and-recurse methods.
struct PrimeLower;

impl SurfaceToPrime for PrimeLower {
    fn rewrite_expr_tuple(
        &mut self,
        _items: Vec<Expr<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error> {
        Err(prime_reject("tuple literal", meta.span))
    }

    fn rewrite_expr_fn_placeholder(
        &mut self,
        _body: Box<Expr<Surface>>,
        _stem: PathSegment,
        _state: PlaceholderState,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error> {
        Err(prime_reject("placeholder lambda", meta.span))
    }

    fn rewrite_expr_op_chain(
        &mut self,
        _kind: crate::ast::OpChainKind<Surface>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error> {
        Err(prime_reject("operator usage", meta.span))
    }

    fn rewrite_expr_rec_call(
        &mut self,
        _callee: PathSegment,
        _args: Vec<CallArg<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error> {
        Err(prime_reject("`rec` call", meta.span))
    }

    fn rewrite_expr_label_value(
        &mut self,
        _labels: Vec<LabelValueLabel<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error> {
        Err(prime_reject("label-value expression", meta.span))
    }

    fn rewrite_expr_elaborator(
        &mut self,
        kind: crate::ast::ElaboratorKind,
        _call: crate::ast::ElaboratorCall<Surface>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error> {
        let label = match kind {
            crate::ast::ElaboratorKind::Access => "field access syntax",
            crate::ast::ElaboratorKind::Filtered => "field update syntax",
        };
        Err(prime_reject(label, meta.span))
    }

    fn rewrite_expr_user_elaborator(
        &mut self,
        name: String,
        _args: Vec<crate::ast::CallArg<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Expr<Prime>, Error> {
        Err(prime_reject(
            &format!("`{name}!` user-elaborator position"),
            meta.span,
        ))
    }

    fn rewrite_expr_ufcs(
        &mut self,
        ufcs: crate::prime::walk::SurfaceUfcs,
    ) -> Result<Expr<Prime>, Error> {
        // Both regular UFCS (`r.>f(args)`) and elaborator-UFCS
        // (`r.>iso!(T)`) are rejected at the Surface → Prime
        // boundary — Kio' admits neither spelling. The diagnostic
        // distinguishes them so the user sees the form they wrote.
        let label = if ufcs.bang.is_some() {
            "elaborator-form UFCS dispatch (`r.>iso!`)"
        } else {
            "UFCS dispatch (`r.>f`)"
        };
        Err(prime_reject(label, ufcs.meta.span))
    }

    fn rewrite_type_label_sugar(
        &mut self,
        _labels: Vec<LabelSugarLabel<Surface>>,
        meta: Meta<Surface>,
    ) -> Result<Type<Prime>, Error> {
        Err(prime_reject("braced label type form", meta.span))
    }

    fn rewrite_type_infer(&mut self, meta: Meta<Surface>) -> Result<Type<Prime>, Error> {
        // The Kio' core does not admit `_` placeholders. Every type position
        // carries an explicit
        // type. The kio-prime binary rejects `_` at parse time the
        // same way it rejects other surface-only forms.
        Err(prime_reject("`_` type placeholder", meta.span))
    }

    fn rewrite_item_labels(&mut self, d: Labels<Surface>) -> Result<Item<Prime>, Error> {
        Err(prime_reject("`labels` declaration", d.meta.span))
    }

    fn rewrite_item_label_forward(
        &mut self,
        forward: crate::ast::LabelForward<Surface>,
    ) -> Result<Item<Prime>, Error> {
        Err(prime_reject(
            "label forwarding declaration",
            forward.meta.span,
        ))
    }

    fn rewrite_type_rec_member_labels(
        &mut self,
        d: Labels<Surface>,
    ) -> Result<crate::ast::TypeRecMember<Prime>, Error> {
        Err(prime_reject("`labels` declaration", d.meta.span))
    }

    fn rewrite_item_equiv(&mut self, e: Equiv<Surface>) -> Result<Item<Prime>, Error> {
        // `equiv` is a surface-only test-runner declaration; not
        // part of Kio'.
        Err(prime_reject("`equiv` declaration", e.meta.span))
    }

    fn rewrite_item_op(&mut self, d: Op<Surface>) -> Result<Item<Prime>, Error> {
        // `op` is surface-only; operator dispatch isn't in Kio'.
        Err(prime_reject("`op` declaration", d.meta.span))
    }

    fn rewrite_item_fold(
        &mut self,
        d: crate::ast::VariadicOperator<Surface>,
    ) -> Result<Item<Prime>, Error> {
        Err(prime_reject("`varop` declaration", d.meta.span))
    }

    fn rewrite_item_rec_group(&mut self, d: RecGroup<Surface>) -> Result<Item<Prime>, Error> {
        Err(prime_reject("`rec(loop)` declaration", d.meta.span))
    }

    fn rewrite_item_elaborator(
        &mut self,
        d: crate::ast::UserElaboratorDef<Surface>,
    ) -> Result<Item<Prime>, Error> {
        Err(prime_reject("`elab` declaration", d.meta.span))
    }

    fn rewrite_item_literal_alias(
        &mut self,
        a: LiteralAlias<Surface>,
    ) -> Result<Item<Prime>, Error> {
        Err(prime_reject("`literal` declaration", a.meta.span))
    }

    fn rewrite_import_op_pattern(&mut self, span: Span) -> Result<(), Error> {
        Err(prime_reject("operator-pattern `import` item", span))
    }

    fn rewrite_import_label(&mut self, span: Span) -> Result<(), Error> {
        Err(prime_reject("label `import` item", span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Diagnostic;
    use crate::pass::parser::parse;

    fn parse_module(src: &str) -> Module<Surface> {
        parse(src).expect("parse")
    }

    fn err_msg(err: Error) -> String {
        match err {
            Error::Parse(Diagnostic { message, .. }) => message,
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn lower_module_passes_kio_prime_program_through() {
        let module = parse_module("module x; fn id[A](x: A) -> A { x }");
        let lowered = lower_module(module).expect("lower_module");
        assert_eq!(lowered.path.segments, vec!["x"]);
        assert_eq!(lowered.items.len(), 1);
    }

    #[test]
    fn lower_module_passes_recursive_type_group_through() {
        let module = parse_module(
            "module x; rec { \
               type A = B; \
               newtype B : A { constructor make; projector read; }; \
             }",
        );
        let lowered = lower_module(module).expect("recursive type group is Kio' syntax");
        let [Item::TypeRecGroup(group)] = lowered.items.as_slice() else {
            panic!("expected one recursive type group")
        };
        assert_eq!(group.members.len(), 2);
    }

    #[test]
    fn lower_module_rejects_tuple_literal() {
        let module = parse_module("module x; fn f() -> . { (.t, .f) }");
        let msg = err_msg(lower_module(module).expect_err("rejects tuple"));
        assert!(msg.contains("tuple literal"), "names form: {msg}");
        assert!(msg.contains("kio-prime"), "frames as kio-prime-only: {msg}");
    }

    #[test]
    fn lower_module_rejects_if_bang_call() {
        let module = parse_module("module x; fn f() -> . { if! .t { () } else { () } }");
        let Some(Item::FnDef(def)) = module.items.first() else {
            panic!("expected function with if! body");
        };
        assert!(matches!(&def.body, Expr::BlockCall { .. }));
        let msg = err_msg(lower_module(module).expect_err("rejects if!"));
        assert!(msg.contains("trailing blocks"), "names form: {msg}");
    }

    #[test]
    fn parser_rejects_braced_label_type_form_before_prime_lower() {
        let msg = err_msg(parse("module x; type T = {f: .};").expect_err("rejects label type"));
        assert!(
            msg.contains("label") && msg.contains("type position"),
            "names form: {msg}"
        );
    }

    #[test]
    fn lower_module_rejects_label_value() {
        let module = parse_module("module x; fn f() -> . { {f = ()} }");
        let msg = err_msg(lower_module(module).expect_err("rejects label-value"));
        assert!(msg.contains("label-value"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_braced_label_import() {
        let module = parse_module("module x; import origin({field});");
        let msg = err_msg(lower_module(module).expect_err("rejects label import"));
        assert!(msg.contains("label `import` item"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_iso() {
        let module = parse_module("module x; fn f() -> . { iso!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects iso!"));
        assert!(msg.contains("`iso!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_into() {
        let module = parse_module("module x; fn f() -> . { into!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects into!"));
        assert!(msg.contains("`into!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_onto() {
        let module = parse_module("module x; fn f() -> . { onto!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects onto!"));
        assert!(msg.contains("`onto!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_match_elaborator() {
        let module = parse_module("module x; fn f[A](v: A) -> . { match!(v, (.() { () })) }");
        let msg = err_msg(lower_module(module).expect_err("rejects match!"));
        assert!(msg.contains("`match!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_labels() {
        let module = parse_module("module x; labels { f: . };");
        let msg = err_msg(lower_module(module).expect_err("rejects labels"));
        assert!(msg.contains("`labels`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_labels_inside_recursive_type_group() {
        let module = parse_module(
            "module x; rec { \
               labels A = { to_b: B }; \
               newtype B : A { constructor make; projector read; }; \
             }",
        );
        let msg = err_msg(lower_module(module).expect_err("rejects grouped labels"));
        assert!(msg.contains("`labels`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_ufcs() {
        let module = parse_module("module x; fn f() -> . { let r = (); r.>f }");
        let msg = err_msg(lower_module(module).expect_err("rejects ufcs"));
        assert!(msg.contains("UFCS"), "names form: {msg}");
        assert!(msg.contains("kio-prime"), "frames as kio-prime-only: {msg}");
    }

    #[test]
    fn lower_module_rejects_elaborator_ufcs() {
        let module = parse_module("module x; fn f() -> . { let r = (); r.>iso! }");
        let msg = err_msg(lower_module(module).expect_err("rejects elaborator-UFCS"));
        assert!(msg.contains("elaborator-form UFCS"), "names form: {msg}");
        assert!(msg.contains("kio-prime"), "frames as kio-prime-only: {msg}");
    }

    #[test]
    fn lower_module_rejects_align() {
        let module = parse_module("module x; fn f() -> . { align!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects align!"));
        assert!(msg.contains("`align!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_atom() {
        let module = parse_module("module x; fn f() -> . { atom!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects atom!"));
        assert!(msg.contains("`atom!`"), "names form: {msg}");
    }

    // ---- Spine-palette rejection ----------------------------------------
    //
    // Each of the nine spine-palette forms is surface-only, just like
    // the algebraic palette; the kio-prime binary rejects every one
    // at the Surface → Prime boundary, naming the form so the user
    // sees which spelling triggered the rejection. The cases below
    // pin each form by name.

    #[test]
    fn lower_module_rejects_reorder_sum() {
        let module = parse_module("module x; fn f() -> . { reorder_sum!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects reorder_sum!"));
        assert!(msg.contains("`reorder_sum!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_reorder_prod() {
        let module = parse_module("module x; fn f() -> . { reorder_prod!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects reorder_prod!"));
        assert!(msg.contains("`reorder_prod!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_narrow_sum() {
        let module = parse_module("module x; fn f() -> . { narrow_sum!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects narrow_sum!"));
        assert!(msg.contains("`narrow_sum!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_narrow_prod() {
        let module = parse_module("module x; fn f() -> . { narrow_prod!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects narrow_prod!"));
        assert!(msg.contains("`narrow_prod!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_widen_sum() {
        let module = parse_module("module x; fn f() -> . { widen_sum!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects widen_sum!"));
        assert!(msg.contains("`widen_sum!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_widen_prod() {
        let module = parse_module("module x; fn f() -> . { widen_prod!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects widen_prod!"));
        assert!(msg.contains("`widen_prod!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_one_sum() {
        let module = parse_module("module x; fn f() -> . { one_sum!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects one_sum!"));
        assert!(msg.contains("`one_sum!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_one_prod() {
        let module = parse_module("module x; fn f() -> . { one_prod!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects one_prod!"));
        assert!(msg.contains("`one_prod!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_rejects_fit() {
        let module = parse_module("module x; fn f() -> . { fit!(()) }");
        let msg = err_msg(lower_module(module).expect_err("rejects fit!"));
        assert!(msg.contains("`fit!`"), "names form: {msg}");
    }

    #[test]
    fn lower_module_passes_host_type_through() {
        // Host items are NOT surface-only — they are opaque Kio'
        // declarations — so `prime::lower` accepts them with no
        // rejection.
        let module = parse_module("module x; host type Map[K][V];");
        let lowered = lower_module(module).expect("lower_module");
        assert_eq!(lowered.items.len(), 1);
        assert!(matches!(lowered.items[0], crate::ast::Item::HostType(_)));
    }

    #[test]
    fn lower_module_passes_host_fn_through() {
        let module = parse_module("module x; host type S role(str); host fn print(s: S) -> .;");
        let lowered = lower_module(module).expect("lower_module");
        assert_eq!(lowered.items.len(), 2);
        assert!(matches!(lowered.items[1], crate::ast::Item::HostFn(_)));
    }

    #[test]
    fn lower_package_file_carries_bridge_glob_through() {
        // The package file's `bridge` glob list is phase-independent and
        // carries verbatim through the kio-prime lowering.
        let package_file =
            crate::pass::parser::parse_package_file("package pkg;\nbridge { pkg; }", None)
                .expect("parse_package_file");
        let lowered = lower_package_file(package_file).expect("lower_package_file");
        assert_eq!(lowered.bridge.expect("bridge block").globs.len(), 1);
    }

    #[test]
    fn lower_module_rejects_import_op_pattern() {
        let module = parse("module x; import m(op _ +);").expect("provider-aware surface parse");
        let msg = err_msg(lower_module(module).expect_err("rejects op-pattern import"));
        assert!(msg.contains("operator-pattern"), "names form: {msg}");
        assert!(msg.contains("kio-prime"), "frames as kio-prime-only: {msg}");
    }

    // ---- `pure` declaration modifiers -----------------------------------
    //
    // Kio' retains `pure` on ordinary functions so its standalone typer
    // can validate pure call chains from the artifact. No other
    // declaration kind admits the modifier in either Kio or Kio'.

    #[test]
    fn lower_module_preserves_pure_fn() {
        let module = parse_module("module x; pure fn f() -> . { () }");
        let lowered = lower_module(module).expect("accepts pure fn");
        let Item::FnDef(def) = &lowered.items[0] else {
            panic!("expected fn")
        };
        assert_eq!(def.purity, crate::ast::Purity::Pure);
    }

    #[test]
    fn parser_rejects_pure_type_alias() {
        let msg = err_msg(parse("module x; pure type T = .;").expect_err("rejects pure type"));
        assert!(
            msg.contains("only on ordinary `fn`"),
            "directed error: {msg}"
        );
    }

    #[test]
    fn parser_rejects_pure_newtype() {
        let msg = err_msg(
            parse("module x; pure newtype W : . { pub constructor mk; pub projector un; };")
                .expect_err("rejects pure newtype"),
        );
        assert!(
            msg.contains("only on ordinary `fn`"),
            "directed error: {msg}"
        );
    }

    #[test]
    fn lower_module_accepts_pub_fn_without_pure() {
        let module = parse_module("module x; pub fn f() -> . { () }");
        let lowered = lower_module(module).expect("lower_module");
        assert_eq!(lowered.items.len(), 1);
    }
}
