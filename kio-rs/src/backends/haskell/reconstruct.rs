//! Haskell backend — body-type reconstruction context.
//!
//! Reconstructs the `Type<Routed>` of a body expression from the Routed
//! IR plus signatures. The native-types body ([`super::native`]) needs
//! the reconstructed type to render each node at its native Haskell type
//! rather than the universal private carrier; GHC's own inference covers the
//! gaps this walk returns `None` for (bare literals, untyped `fn`
//! value-params, indirect-call results) at the typed boundaries that
//! carry a type.
//!
//! The walk itself is the cross-cutting [`crate::backends::reconstruct`]
//! driver, generic over the native-HKT family; this module supplies the
//! Haskell-specific [`ReconProfile`] hooks (Haskell is the family's sole
//! member today). Haskell's native-eligible packages carry no erased
//! `comptime` type aliases (Kio newtypes resolve through
//! [`super::skin`]'s `resolve_newtype`), so its projection normalisation
//! is identity and its resolved-call return type is the plain type-arg
//! substitution — it needs none of the returned-fn source recovery the
//! hook leaves room for. Haskell-specific newtype-member results and the
//! existential CPS-projector apply are handled in
//! [`ReconProfile::reconstruct_other`]. Each `None` return is a reconstruction
//! gap the caller copes with, never a silent erasure.

use std::collections::{BTreeSet, HashMap};

use crate::ast::{Expr, ImportKind, Item, Kind, Routed, Signature, SignatureParam, Type};
use crate::backends::reconstruct::{ReconProfile, build_typearg_subst_from_sig};
use crate::pass::resolve::Package;

use super::skin::{NewtypeResolution, instantiate_newtype_payload_in, resolve_newtype_in};

/// Substitute a `tparam-name → type` map into a type. Re-exported from the
/// shared [`crate::backends::reconstruct`] core for the native emitter's
/// carrier-payload expansion (`super::reconstruct::apply_subst`), and used
/// by this module's resolved-call return-type hook.
pub use crate::backends::reconstruct::apply_subst;

/// The reconstruction context: the surrounding fn's value-param types,
/// seeded once and queried for `LowBoundRef`s the `locals` map does not
/// carry.
pub struct TypeRecon<'a> {
    /// `param-name → declared type` for the surrounding fn's value
    /// params, seeded at construction. A `let`-local overlays this via
    /// the `locals` map threaded through the walk.
    bound_param_tys: HashMap<String, Type<Routed>>,
    type_params: BTreeSet<String>,
    package: &'a Package<Routed>,
    resolution: &'a NewtypeResolution,
    module: &'a str,
}

impl<'a> TypeRecon<'a> {
    pub(super) fn empty(
        package: &'a Package<Routed>,
        resolution: &'a NewtypeResolution,
        module: &'a str,
    ) -> Self {
        TypeRecon {
            bound_param_tys: HashMap::new(),
            type_params: BTreeSet::new(),
            package,
            resolution,
            module,
        }
    }

    /// Build a reconstruction context, seeding `bound_param_tys` from a
    /// signature's typed value params (the closure / fn binders in scope
    /// for its body).
    pub(super) fn new(
        sig: &Signature<Routed>,
        package: &'a Package<Routed>,
        resolution: &'a NewtypeResolution,
        module: &'a str,
    ) -> Self {
        let bound_param_tys = sig
            .params
            .iter()
            .filter_map(|p| match p {
                SignatureParam::Value(v) => v.ty.as_ref().map(|ty| (v.name.clone(), ty.clone())),
                SignatureParam::Type(_) => None,
            })
            .collect();
        let type_params = sig
            .params
            .iter()
            .filter_map(|param| match param {
                SignatureParam::Type(param) => Some(param.name.clone()),
                SignatureParam::Value(_) => None,
            })
            .collect();
        TypeRecon {
            bound_param_tys,
            type_params,
            package,
            resolution,
            module,
        }
    }

    /// Push one binder's reconstructed type into the param env, returning
    /// the prior binding to restore. Used by the native emitter when it
    /// descends into a closure body whose params carry types the
    /// signature did not (inferred from the body's call sites).
    pub fn bind(&mut self, name: &str, ty: Type<Routed>) -> Option<Type<Routed>> {
        self.bound_param_tys.insert(name.to_owned(), ty)
    }

    /// Restore a binding saved by [`Self::bind`].
    pub fn restore(&mut self, name: &str, prior: Option<Type<Routed>>) {
        match prior {
            Some(ty) => {
                self.bound_param_tys.insert(name.to_owned(), ty);
            }
            None => {
                self.bound_param_tys.remove(name);
            }
        }
    }

    /// Enter the type binders of a nested closure, returning the names that
    /// this scope newly inserted so an enclosing same-name binder survives.
    pub(super) fn push_type_params(&mut self, sig: &Signature<Routed>) -> Vec<String> {
        sig.params
            .iter()
            .filter_map(|param| match param {
                SignatureParam::Type(param) if self.type_params.insert(param.name.clone()) => {
                    Some(param.name.clone())
                }
                SignatureParam::Type(_) | SignatureParam::Value(_) => None,
            })
            .collect()
    }

    /// Leave a nested closure's type-binder scope using the token returned by
    /// [`Self::push_type_params`].
    pub(super) fn pop_type_params(&mut self, inserted: Vec<String>) {
        for name in inserted {
            self.type_params.remove(&name);
        }
    }

    /// The reconstructed value type of `expr`, with a fresh `let`-local
    /// env. `None` is a reconstruction gap (a bare literal, an untyped
    /// closure param, an indirect-call result) the caller copes with.
    pub fn value_type(&self, expr: &Expr<Routed>) -> Option<Type<Routed>> {
        let mut locals = HashMap::new();
        ReconProfile::value_type_with_locals(self, expr, &mut locals)
    }

    /// The exact reconstructed type of a currently bound value.  The native
    /// emitter uses this at a visible type-application boundary so a monadic
    /// bind that returns another `forall` can carry the required rank-N type
    /// application explicitly.
    pub(super) fn bound_value_type(&self, name: &str) -> Option<Type<Routed>> {
        self.bound_param_tys.get(name).cloned()
    }

    fn newtype_member_type(
        &self,
        declaring_module: Option<&str>,
        newtype: &str,
        member: &str,
        type_args: &[Type<Routed>],
        expected: NewtypeMemberExpectation,
        span: crate::span::Span,
    ) -> Option<Type<Routed>> {
        let lookup = match declaring_module {
            Some(module_path) => {
                let mut segments = module_path
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                segments.push(newtype.to_owned());
                Type::synth_path(segments, Vec::new(), span)
            }
            None => Type::synth_path(vec![newtype.to_owned()], Vec::new(), span),
        };
        let (declaration, owner) =
            resolve_newtype_in(&lookup, self.package, self.resolution, Some(self.module))?;

        let is_constructor = member == declaration.constructor.name;
        let is_projector = member == declaration.projector.name;
        match expected {
            NewtypeMemberExpectation::Constructor if !is_constructor => return None,
            NewtypeMemberExpectation::Projector if !is_projector => return None,
            NewtypeMemberExpectation::Either if !is_constructor && !is_projector => return None,
            _ => {}
        }

        let universal_args = type_args.get(..declaration.type_params.len())?;
        let caller = &self.package.module(self.module)?.module;
        let caller_scope = self
            .type_params
            .iter()
            .map(|name| (name.clone(), Kind::Star))
            .collect::<HashMap<_, _>>();
        let universal_args = universal_args
            .iter()
            .map(|arg| {
                crate::pass::resolve::qualify_routed_contract_type_in_module(
                    arg,
                    caller,
                    &caller_scope,
                )
            })
            .collect();
        let mut exact_segments = owner
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        exact_segments.push(newtype.to_owned());
        let exact_reference = Type::synth_path(exact_segments, universal_args, span);

        if is_constructor {
            return Some(exact_reference);
        }
        if !declaration.existential_params.is_empty() {
            return None;
        }
        instantiate_newtype_payload_in(
            declaration,
            &exact_reference,
            self.package,
            &owner,
            Some(self.module),
            &self.type_params,
        )
    }

    fn module_fn_return_type(&self, name: &str) -> Option<Type<Routed>> {
        let current = self.package.module(self.module)?;
        for import_decl in &current.module.imports {
            if let ImportKind::Selective { items, from } = &import_decl.kind
                && items.iter().any(|item| item.as_name() == Some(name))
            {
                let module = from
                    .segments
                    .iter()
                    .map(|segment| segment.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                if let Some(ret) = self.fn_return_type_in(&module, name) {
                    return Some(ret);
                }
            }
        }
        self.fn_return_type_in(self.module, name)
    }

    fn fn_return_type_in(&self, module: &str, name: &str) -> Option<Type<Routed>> {
        self.package
            .module(module)?
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(def.ret.clone()),
                _ => None,
            })
    }
}

#[derive(Clone, Copy)]
enum NewtypeMemberExpectation {
    Constructor,
    Projector,
    Either,
}

impl ReconProfile for TypeRecon<'_> {
    fn bound_param_ty(&self, name: &str) -> Option<Type<Routed>> {
        self.bound_param_tys.get(name).cloned()
    }

    /// The monomorphised return type of a resolved call: the declared
    /// return type with the signature's type-params substituted by the
    /// call's type-args. Haskell needs no returned-fn source recovery, so
    /// the value-args are not consulted.
    fn resolved_call_return_type(
        &self,
        sig: &Signature<Routed>,
        type_args: &[Type<Routed>],
        _args: &[Expr<Routed>],
        ret_ty: &Type<Routed>,
    ) -> Type<Routed> {
        let subst = build_typearg_subst_from_sig(sig, type_args);
        apply_subst(ret_ty, &subst)
    }

    fn accept_direct_enriched_slot(&self, _slot_ty: &Type<Routed>) -> bool {
        true
    }

    fn normalize_target_ty(&self, ty: &Type<Routed>) -> Type<Routed> {
        ty.clone()
    }

    fn enriched_field_payload_type(&self, field_ty: &Type<Routed>) -> Option<Type<Routed>> {
        let (newtype, declaring_module) =
            resolve_newtype_in(field_ty, self.package, self.resolution, Some(self.module))?;
        instantiate_newtype_payload_in(
            newtype,
            field_ty,
            self.package,
            &declaring_module,
            Some(self.module),
            &self.type_params,
        )
    }

    fn module_fn_value_type(
        &self,
        mangled: &str,
        sig: &Signature<Routed>,
        span: crate::span::Span,
    ) -> Option<Type<Routed>> {
        let ret = self.module_fn_return_type(mangled)?;
        Some(sig.signature_ty(ret, span))
    }

    /// An existential CPS-projector apply has no `ret_ty` of its own: its
    /// result is the continuation's return type `R` (`<proj>(v)(k)`
    /// evaluates to whatever `k` returns). When the continuation is the
    /// literal `fn` it usually is, that is its declared `ret_ty`.
    fn reconstruct_other(
        &self,
        expr: &Expr<Routed>,
        _locals: &mut HashMap<String, Type<Routed>>,
    ) -> Option<Type<Routed>> {
        match expr {
            Expr::LowNewtypeCtor {
                newtype,
                member,
                type_args,
                meta,
                ..
            } => self.newtype_member_type(
                None,
                newtype,
                member,
                type_args,
                NewtypeMemberExpectation::Constructor,
                meta.span,
            ),
            Expr::LowNewtypeProj {
                newtype,
                member,
                type_args,
                meta,
                ..
            } => self.newtype_member_type(
                None,
                newtype,
                member,
                type_args,
                NewtypeMemberExpectation::Projector,
                meta.span,
            ),
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                meta,
                ..
            } => self.newtype_member_type(
                Some(module_path),
                newtype,
                member,
                type_args,
                NewtypeMemberExpectation::Either,
                meta.span,
            ),
            Expr::LowCpsProjectorApply {
                continuation_ty, ..
            } => match continuation_ty.peel_leading_foralls().1 {
                Type::Function { ret, .. } => Some((**ret).clone()),
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::ast::Item;
    use crate::backends::skin::canonical_type_string;
    use crate::pass::full::FullPipeline;
    use crate::pipeline::Pipeline;

    fn routed_package(sources: &[(&str, &str)]) -> Package<Routed> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    crate::pass::parser::parse(source)
                        .unwrap_or_else(|error| panic!("parse `{path}`: {error:?}")),
                )
            })
            .collect();
        let (modules, _) =
            FullPipeline::lower_package(parsed, None).expect("lower surface package");
        let package = Package::build(Path::new(""), modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = FullPipeline::typecheck(&package).expect("typecheck surface package");
        crate::pass::recover_to_low::lower(&crate::pass::structural_recovery::recover_package(
            &prime,
        ))
    }

    fn function<'a>(
        package: &'a Package<Routed>,
        module: &str,
        name: &str,
    ) -> &'a crate::ast::FnDef<Routed> {
        package
            .module(module)
            .unwrap_or_else(|| panic!("missing module `{module}`"))
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(definition) if definition.name == name => Some(definition),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing function `{module}.{name}`"))
    }

    #[test]
    fn field_payload_keeps_forall_scope_and_declaring_module_identity() {
        let package = routed_package(&[
            (
                "owner.kio",
                "module owner; \
                 pub newtype Token : . { constructor mk_token; projector un_token; }; \
                 pub newtype Field[A] : [T] T -> (A & Token) { \
                     constructor mk_field; projector un_field; \
                 };",
            ),
            (
                "actual.kio",
                "module actual; \
                 pub newtype Token : (. & .) { constructor mk_token; projector un_token; };",
            ),
            (
                "consumer.kio",
                "module consumer; \
                 import owner(Field); \
                 import actual(Token);",
            ),
        ]);
        let resolution = NewtypeResolution::build(&package);
        let sig = Signature::new(Vec::new());
        let recon = TypeRecon::new(&sig, &package, &resolution, "consumer");
        let span = crate::span::Span::new(0, 0);
        let field = Type::synth_path(
            vec!["Field".to_owned()],
            vec![Type::synth_path(vec!["Token".to_owned()], Vec::new(), span)],
            span,
        );

        let payload =
            ReconProfile::enriched_field_payload_type(&recon, &field).expect("exact field newtype");

        assert_eq!(
            canonical_type_string(&payload),
            "forall T. fn1(T) -> (actual.Token & owner.Token)"
        );
    }

    #[test]
    fn newtype_members_reconstruct_exact_declaring_identity() {
        let package = routed_package(&[
            (
                "owner.kio",
                "module owner; \
                 pub newtype Box[A] : A { \
                     pub constructor mk_box; pub projector un_box; \
                 };",
            ),
            (
                "decoy.kio",
                "module decoy; \
                 pub newtype Box[A] : . { \
                     pub constructor mk_box; pub projector un_box; \
                 };",
            ),
            (
                "consumer.kio",
                "module consumer; \
                 import owner as selected; \
                 import decoy as other; \
                 fn qualified_wrap[A](value: A) -> selected.Box(A) { \
                     selected.Box.mk_box(A, value) \
                 } \
                 fn qualified_unwrap[A](value: selected.Box(A)) -> A { \
                     selected.Box.un_box(A, value) \
                 } \
                 fn preserve_decoy[A](value: other.Box(A)) -> other.Box(A) { value }",
            ),
            (
                "local.kio",
                "module local; \
                 newtype Box[A] : A { constructor mk_box; projector un_box; }; \
                 fn local_wrap[A](value: A) -> Box(A) { Box.mk_box(A, value) } \
                 fn local_unwrap[A](value: Box(A)) -> A { Box.un_box(A, value) }",
            ),
        ]);
        let resolution = NewtypeResolution::build(&package);

        for (module, function_name, expected_variant, expected_ty) in [
            ("consumer", "qualified_wrap", "qualified", "owner.Box(A)"),
            ("consumer", "qualified_unwrap", "qualified", "A"),
            ("local", "local_wrap", "constructor", "local.Box(A)"),
            ("local", "local_unwrap", "projector", "A"),
        ] {
            let definition = function(&package, module, function_name);
            match (expected_variant, &definition.body) {
                ("qualified", Expr::LowQualifiedNewtypeMember { .. })
                | ("constructor", Expr::LowNewtypeCtor { .. })
                | ("projector", Expr::LowNewtypeProj { .. }) => {}
                _ => panic!("unexpected lowered body for `{module}.{function_name}`"),
            }
            let recon = TypeRecon::new(&definition.sig, &package, &resolution, module);
            let reconstructed = recon
                .value_type(&definition.body)
                .unwrap_or_else(|| panic!("reconstruct `{module}.{function_name}`"));
            assert_eq!(canonical_type_string(&reconstructed), expected_ty);
        }
    }

    #[test]
    fn qualified_member_records_the_selected_owner_before_reconstruction() {
        let package = routed_package(&[
            (
                "actual.kio",
                "module actual; \
                 pub newtype Box[A] : . { \
                     pub constructor mk_box; pub projector un_box; \
                 };",
            ),
            (
                "selected.kio",
                "module selected; \
                 pub newtype Box[A] : A { \
                     pub constructor mk_box; pub projector un_box; \
                 };",
            ),
            (
                "consumer.kio",
                "module consumer; \
                 import selected as actual; \
                 import actual as selected; \
                 fn wrap[A](value: A) -> actual.Box(A) { \
                     actual.Box.mk_box(A, value) \
                 }",
            ),
        ]);
        let definition = function(&package, "consumer", "wrap");
        let Expr::LowQualifiedNewtypeMember { module_path, .. } = &definition.body else {
            panic!("unexpected lowered body for `consumer.wrap`");
        };

        let resolution = NewtypeResolution::build(&package);
        let recon = TypeRecon::new(&definition.sig, &package, &resolution, "consumer");
        let reconstructed = recon
            .value_type(&definition.body)
            .expect("reconstruct qualified constructor");
        assert_eq!(canonical_type_string(&reconstructed), "selected.Box(A)");
        assert_eq!(module_path, "selected");
    }

    #[test]
    fn qualified_label_member_keeps_declaring_owner_despite_alias_collision() {
        let package = routed_package(&[
            (
                "control.kio",
                include_str!("../../../../test-data/poc/elab/workdir/control.kio"),
            ),
            ("testapi.kio", "module testapi; host type Bool role(bool);"),
            (
                "selected.kio",
                "module selected; import testapi(Bool); pub labels Card = { flag: Bool };",
            ),
            ("decoy.kio", "module decoy; pub labels Card = { flag: . };"),
            (
                "consumer.kio",
                "module consumer; import control(if); import testapi(Bool); import decoy as selected; \
                 import selected as chosen; fn choose(card: chosen.Card) -> Bool { \
                 if! card.?{chosen.flag} { .t } else { .f } }",
            ),
        ]);
        let definition = function(&package, "consumer", "choose");
        let Expr::Let { body, .. } = &definition.body else {
            panic!("field access should bind its receiver");
        };
        let Expr::Let { value, .. } = body.as_ref() else {
            panic!("field access should bind its projected payload");
        };
        let Expr::LowQualifiedNewtypeMember { module_path, .. } = value.as_ref() else {
            panic!("field access should project through the generated label newtype");
        };
        assert_eq!(module_path, "selected");

        let resolution = NewtypeResolution::build(&package);
        let recon = TypeRecon::new(&definition.sig, &package, &resolution, "consumer");
        let reconstructed = recon
            .value_type(value)
            .expect("reconstruct qualified label projection");
        assert_eq!(canonical_type_string(&reconstructed), "testapi.Bool");
    }

    #[test]
    fn module_fn_values_reconstruct_local_and_imported_polytypes() {
        let package = routed_package(&[
            (
                "owner.kio",
                "module owner; pub fn identity[A](value: A) -> A { value }",
            ),
            (
                "local.kio",
                "module local; fn local_identity[A](value: A) -> A { value }",
            ),
            ("consumer.kio", "module consumer; import owner(identity);"),
        ]);
        let resolution = NewtypeResolution::build(&package);
        let span = crate::span::Span::new(0, 0);
        let fn_sig = |module: &str, name: &str| {
            package
                .module(module)
                .expect("module")
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FnDef(def) if def.name == name => Some(def.sig.clone()),
                    _ => None,
                })
                .expect("function signature")
        };
        let value_ref = |name: &str, sig: Signature<Routed>| Expr::LowModuleFnValueRef {
            occurrence: Default::default(),
            mangled: name.to_owned(),
            sig,
            meta: crate::ast::Meta::new(span),
            ext: (),
        };

        let local = TypeRecon::empty(&package, &resolution, "local")
            .value_type(&value_ref(
                "local_identity",
                fn_sig("local", "local_identity"),
            ))
            .expect("local function value type");
        let imported = TypeRecon::empty(&package, &resolution, "consumer")
            .value_type(&value_ref("identity", fn_sig("owner", "identity")))
            .expect("imported function value type");

        assert_eq!(canonical_type_string(&local), "forall A. fn1(A) -> A");
        assert_eq!(canonical_type_string(&imported), "forall A. fn1(A) -> A");
    }

    #[test]
    fn returned_forall_type_application_reconstructs_its_exact_result() {
        let package = routed_package(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn produce(_unit: .) -> [A] A; \
             fn caller() -> N { produce()(N) }",
        )]);
        let definition = function(&package, "test", "caller");
        assert!(matches!(definition.body, Expr::LowTypeApplication { .. }));

        let resolution = NewtypeResolution::build(&package);
        let reconstructed = TypeRecon::new(&definition.sig, &package, &resolution, "test")
            .value_type(&definition.body)
            .expect("a routed type application carries its exact result structurally");

        assert_eq!(canonical_type_string(&reconstructed), "test.N");
    }
}
